// ABOUTME: Generates local renderer descriptions from the admitted action contract.
// ABOUTME: Validates public identity text and escapes it before XML serialization.
use super::{
    Service, SoapError,
    contract::{input_variable, variable_type},
};
use earph_core::remote_volume::{MAX_REMOTE_VOLUME, MIN_REMOTE_VOLUME_DB};
use std::{collections::BTreeSet, fmt::Write};

pub fn device_description(udn: &str, friendly_name: &str) -> Result<String, SoapError> {
    let uuid = udn
        .strip_prefix("uuid:")
        .ok_or(SoapError::InvalidArguments)?;
    if uuid.len() != 36
        || !uuid.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
        || friendly_name.is_empty()
        || friendly_name.len() > 128
        || !friendly_name.chars().all(super::soap::valid_xml_character)
    {
        return Err(SoapError::InvalidArguments);
    }
    let mut xml = format!(
        r#"<?xml version="1.0" encoding="utf-8"?><root xmlns="urn:schemas-upnp-org:device-1-0"><specVersion><major>1</major><minor>0</minor></specVersion><device><deviceType>urn:schemas-upnp-org:device:MediaRenderer:1</deviceType><friendlyName>{}</friendlyName><manufacturer>Earph</manufacturer><modelName>Earph</modelName><UDN>{udn}</UDN><serviceList>"#,
        quick_xml::escape::escape(friendly_name)
    );
    for service in Service::ALL {
        let id = service
            .namespace()
            .rsplit(':')
            .nth(1)
            .expect("static service namespace");
        let path = service.slug();
        let event_path = service.event_path();
        write!(xml, "<service><serviceType>{}</serviceType><serviceId>urn:upnp-org:serviceId:{id}</serviceId><SCPDURL>/upnp/{path}/scpd.xml</SCPDURL><controlURL>/upnp/{path}/control</controlURL><eventSubURL>{event_path}</eventSubURL></service>", service.namespace()).unwrap();
    }
    xml.push_str("</serviceList></device></root>");
    Ok(xml)
}

pub fn service_description(service: Service) -> String {
    let mut xml = String::from(
        r#"<?xml version="1.0" encoding="utf-8"?><scpd xmlns="urn:schemas-upnp-org:service-1-0"><specVersion><major>1</major><minor>0</minor></specVersion><actionList>"#,
    );
    let mut variables = BTreeSet::new();
    for action in service.actions() {
        write!(xml, "<action><name>{}</name><argumentList>", action.name).unwrap();
        for &name in action.input {
            let variable = input_variable(action, name);
            variables.insert(variable);
            write!(xml, "<argument><name>{name}</name><direction>in</direction><relatedStateVariable>{variable}</relatedStateVariable></argument>").unwrap();
        }
        for &(name, variable) in action.output {
            variables.insert(variable);
            write!(xml, "<argument><name>{name}</name><direction>out</direction><relatedStateVariable>{variable}</relatedStateVariable></argument>").unwrap();
        }
        xml.push_str("</argumentList></action>");
    }
    if service != Service::ConnectionManager {
        variables.insert("LastChange");
    }
    xml.push_str("</actionList><serviceStateTable>");
    for variable in variables {
        let evented = variable == "LastChange"
            || (service == Service::ConnectionManager
                && [
                    "SourceProtocolInfo",
                    "SinkProtocolInfo",
                    "CurrentConnectionIDs",
                ]
                .contains(&variable));
        write!(
            xml,
            "<stateVariable sendEvents=\"{}\"><name>{variable}</name><dataType>{}</dataType>",
            if evented { "yes" } else { "no" },
            variable_type(variable)
        )
        .unwrap();
        match variable {
            "Volume" => write!(xml, "<allowedValueRange><minimum>0</minimum><maximum>{MAX_REMOTE_VOLUME}</maximum><step>1</step></allowedValueRange>").unwrap(),
            "VolumeDB" => write!(xml, "<allowedValueRange><minimum>{MIN_REMOTE_VOLUME_DB}</minimum><maximum>0</maximum><step>1</step></allowedValueRange>").unwrap(),
            "A_ARG_TYPE_Channel" => xml.push_str("<allowedValueList><allowedValue>Master</allowedValue></allowedValueList>"),
            _ => {},
        }
        xml.push_str("</stateVariable>");
    }
    xml.push_str("</serviceStateTable></scpd>");
    xml
}
