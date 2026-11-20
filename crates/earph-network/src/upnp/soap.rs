// ABOUTME: Decodes bounded SOAP actions against their exact service and argument contracts.
// ABOUTME: Rejects external entities and malformed authority before any Core operation exists.
use super::{Service, SoapError, SoapRequest};
use quick_xml::{events::Event, name::ResolveResult, reader::NsReader};

fn namespace_uri(namespace: ResolveResult<'_>) -> Result<std::borrow::Cow<'_, str>, SoapError> {
    match namespace {
        ResolveResult::Bound(ns) => quick_xml::escape::unescape(
            std::str::from_utf8(ns.into_inner()).map_err(|_| SoapError::MalformedXml)?,
        )
        .map_err(|_| SoapError::MalformedXml),
        ResolveResult::Unbound => Ok(std::borrow::Cow::Borrowed("")),
        ResolveResult::Unknown(_) => Err(SoapError::MalformedXml),
    }
}

const SOAP: &[u8] = b"http://schemas.xmlsoap.org/soap/envelope/";
const ENCODING: &str = "http://schemas.xmlsoap.org/soap/encoding/";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Namespace {
    Soap,
    Service,
    None,
    Other,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Element {
    Envelope,
    Body,
    Action,
    Argument,
    Extension,
}

pub(super) fn valid_xml_character(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r' | '\u{20}'..='\u{d7ff}' | '\u{e000}'..='\u{fffd}' | '\u{10000}'..='\u{10ffff}')
}

fn whitespace(text: &str) -> bool {
    text.chars().all(|c| matches!(c, ' ' | '\t' | '\n' | '\r'))
}

fn append_text(stack: &[Element], request: &mut SoapRequest, text: &str) -> Result<(), SoapError> {
    if !text.chars().all(valid_xml_character) {
        return Err(SoapError::MalformedXml);
    }
    match stack.last() {
        Some(Element::Argument) => {
            let (name, value) = request
                .arguments
                .last_mut()
                .ok_or(SoapError::MalformedXml)?;
            let cap = match name.as_str() {
                "CurrentURI" => 8192,
                "CurrentURIMetaData" => 65536,
                _ => 8192,
            };
            if text.len() > cap - value.len() {
                return Err(SoapError::ResourceBudgetExceeded);
            }
            value.push_str(text);
        }
        Some(Element::Extension) => {}
        _ if whitespace(text) => {}
        _ => return Err(SoapError::MalformedXml),
    }
    Ok(())
}

pub fn decode_soap(
    service: Service,
    soap_action: &str,
    body: &[u8],
) -> Result<SoapRequest, SoapError> {
    if body.len() > 262144 {
        return Err(SoapError::ResourceBudgetExceeded);
    }
    let header = soap_action
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .ok_or(SoapError::InvalidAction)?;
    let (namespace, name) = header.split_once('#').ok_or(SoapError::InvalidAction)?;
    if namespace != service.namespace() {
        return Err(SoapError::InvalidAction);
    }
    let spec = service
        .actions()
        .iter()
        .find(|spec| spec.name == name)
        .ok_or(SoapError::InvalidAction)?;
    let (name, expected) = (spec.name, spec.input);
    let text = std::str::from_utf8(body).map_err(|_| SoapError::MalformedXml)?;
    if !text.chars().all(valid_xml_character) {
        return Err(SoapError::MalformedXml);
    }
    let mut reader = NsReader::from_str(text);
    reader.config_mut().expand_empty_elements = true;
    reader.config_mut().check_comments = true;
    let mut request = SoapRequest {
        service,
        name,
        arguments: Vec::with_capacity(expected.len()),
    };
    let mut stack = Vec::with_capacity(16);
    let mut declaration_seen = false;
    let (mut elements, mut envelope, mut body_seen, mut action_seen, mut encoded) =
        (0, false, false, false, false);
    loop {
        let (namespace, event) = reader
            .read_resolved_event()
            .map_err(|_| SoapError::MalformedXml)?;
        let uri = namespace_uri(namespace)?;
        let namespace = if uri.as_bytes() == SOAP {
            Namespace::Soap
        } else if uri == service.namespace() {
            Namespace::Service
        } else if uri.is_empty() {
            Namespace::None
        } else {
            Namespace::Other
        };
        match event {
            Event::Start(element) => {
                elements += 1;
                if elements > 8192 || stack.len() == 16 {
                    return Err(SoapError::ResourceBudgetExceeded);
                }
                for (index, attribute) in element.attributes().with_checks(true).enumerate() {
                    if index == 32 {
                        return Err(SoapError::ResourceBudgetExceeded);
                    }
                    let attribute = attribute.map_err(|_| SoapError::MalformedXml)?;
                    let value = attribute
                        .decoded_and_normalized_value(
                            quick_xml::XmlVersion::Explicit1_0,
                            reader.decoder(),
                        )
                        .map_err(|_| SoapError::MalformedXml)?;
                    if !value.chars().all(valid_xml_character) {
                        return Err(SoapError::MalformedXml);
                    }
                    let (ns, local) = reader.resolver().resolve_attribute(attribute.key);
                    let uri = namespace_uri(ns)?;
                    // Namespace aliases may not duplicate an expanded attribute name.
                    for previous in element.attributes().take(index) {
                        let previous = previous.map_err(|_| SoapError::MalformedXml)?;
                        let (previous_ns, previous_local) =
                            reader.resolver().resolve_attribute(previous.key);
                        if previous_local == local && namespace_uri(previous_ns)? == uri {
                            return Err(SoapError::MalformedXml);
                        }
                    }
                    if stack.is_empty()
                        && uri.as_bytes() == SOAP
                        && local.as_ref() == b"encodingStyle"
                    {
                        if value != ENCODING {
                            return Err(SoapError::MalformedXml);
                        }
                        encoded = true;
                    }
                }
                let local = element.local_name();
                let kind = match stack.last() {
                    None if !envelope
                        && namespace == Namespace::Soap
                        && local.as_ref() == b"Envelope" =>
                    {
                        envelope = true;
                        Element::Envelope
                    }
                    Some(Element::Envelope)
                        if !body_seen
                            && namespace == Namespace::Soap
                            && local.as_ref() == b"Body" =>
                    {
                        body_seen = true;
                        Element::Body
                    }
                    Some(Element::Body) if !action_seen => {
                        if namespace != Namespace::Service || local.as_ref() != name.as_bytes() {
                            return Err(SoapError::InvalidAction);
                        }
                        action_seen = true;
                        Element::Action
                    }
                    Some(Element::Action) if namespace == Namespace::None => {
                        let argument = std::str::from_utf8(local.as_ref())
                            .map_err(|_| SoapError::MalformedXml)?;
                        if expected.get(request.arguments.len()).copied() != Some(argument) {
                            return Err(SoapError::InvalidArguments);
                        }
                        request.arguments.push((argument.to_owned(), String::new()));
                        Element::Argument
                    }
                    Some(Element::Extension) => Element::Extension,
                    Some(Element::Envelope | Element::Action) if namespace == Namespace::Other => {
                        Element::Extension
                    }
                    _ => return Err(SoapError::MalformedXml),
                };
                stack.push(kind);
            }
            Event::End(_) => {
                stack.pop().ok_or(SoapError::MalformedXml)?;
            }
            Event::Text(text) => append_text(
                &stack,
                &mut request,
                &text.xml10_content().map_err(|_| SoapError::MalformedXml)?,
            )?,
            Event::CData(text) => append_text(
                &stack,
                &mut request,
                &text.xml10_content().map_err(|_| SoapError::MalformedXml)?,
            )?,
            Event::GeneralRef(reference) => {
                let decoded = reference.decode().map_err(|_| SoapError::MalformedXml)?;
                let escaped = format!("&{decoded};");
                let value =
                    quick_xml::escape::unescape(&escaped).map_err(|_| SoapError::MalformedXml)?;
                append_text(&stack, &mut request, &value)?;
            }
            Event::Decl(declaration)
                if !envelope
                    && !declaration_seen
                    && text.trim_start_matches('\u{feff}').starts_with("<?xml") =>
            {
                declaration_seen = true;
                if declaration
                    .version()
                    .map_err(|_| SoapError::MalformedXml)?
                    .as_ref()
                    != b"1.0"
                {
                    return Err(SoapError::MalformedXml);
                }
                if let Some(encoding) = declaration.encoding()
                    && !encoding
                        .map_err(|_| SoapError::MalformedXml)?
                        .eq_ignore_ascii_case(b"utf-8")
                {
                    return Err(SoapError::MalformedXml);
                }
            }
            Event::Comment(_) | Event::PI(_) => {}
            Event::Eof if stack.is_empty() && envelope && body_seen && action_seen && encoded => {
                break;
            }
            _ => return Err(SoapError::MalformedXml),
        }
    }
    if request.arguments.len() != expected.len() {
        return Err(SoapError::InvalidArguments);
    }
    Ok(request)
}
