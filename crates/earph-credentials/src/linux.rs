// ABOUTME: Stores one committed document through explicit Secret Service calls.
// ABOUTME: Never unlocks, invokes prompts, creates collections, or retries mutations.
use crate::{StoreError, decode, wire::Action};
use std::{collections::HashMap, time::Duration};
use zbus::{
    blocking::{Connection, Proxy, connection::Builder},
    zvariant::{OwnedObjectPath, OwnedValue, Value},
};
const SERVICE: &str = "org.freedesktop.secrets";
const ROOT: &str = "/org/freedesktop/secrets";
const INTERFACE: &str = "org.freedesktop.Secret.Service";
const ATTR_APPLICATION: &str = "earph";
const ATTR_DOCUMENT: &str = "sendspin-committed-v1";
type Secret = (OwnedObjectPath, Vec<u8>, Vec<u8>, String);
struct Store {
    connection: Connection,
    owner: String,
    collection: OwnedObjectPath,
    session: OwnedObjectPath,
}
impl Store {
    fn proxy<'a>(&'a self, path: &'a str, interface: &'a str) -> Result<Proxy<'a>, StoreError> {
        Proxy::new(&self.connection, self.owner.as_str(), path, interface)
            .map_err(|_| StoreError::Unavailable)
    }
    fn attributes() -> HashMap<&'static str, &'static str> {
        HashMap::from([
            ("application", ATTR_APPLICATION),
            ("document", ATTR_DOCUMENT),
        ])
    }
    fn open(address: &str) -> Result<Self, StoreError> {
        let connection = Builder::address(address)
            .map_err(|_| StoreError::Unavailable)?
            .method_timeout(Duration::from_secs(2))
            .build()
            .map_err(|_| StoreError::Unavailable)?;
        // Resolve once without auto-activation, then pin every call to that unique service owner.
        let owner: String = Proxy::new(
            &connection,
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
        )
        .map_err(|_| StoreError::Unavailable)?
        .call("GetNameOwner", &(SERVICE,))
        .map_err(|_| StoreError::Unavailable)?;
        let service = Proxy::new(&connection, owner.as_str(), ROOT, INTERFACE)
            .map_err(|_| StoreError::Unavailable)?;
        let collection: OwnedObjectPath = service
            .call("ReadAlias", &("default",))
            .map_err(|_| StoreError::Unavailable)?;
        if collection.as_str() == "/" {
            return Err(StoreError::Unavailable);
        }
        let collection_proxy = Proxy::new(
            &connection,
            owner.as_str(),
            collection.as_str(),
            "org.freedesktop.Secret.Collection",
        )
        .map_err(|_| StoreError::Unavailable)?;
        let locked: bool = collection_proxy
            .get_property("Locked")
            .map_err(|_| StoreError::Unavailable)?;
        if locked {
            return Err(StoreError::Locked);
        }
        let (_, session): (OwnedValue, OwnedObjectPath) = service
            .call("OpenSession", &("plain", Value::from("")))
            .map_err(|_| StoreError::Unavailable)?;
        if session.as_str() == "/" {
            return Err(StoreError::Unavailable);
        }
        drop(collection_proxy);
        drop(service);
        Ok(Self {
            connection,
            owner,
            collection,
            session,
        })
    }
    fn locate(&self) -> Result<Option<OwnedObjectPath>, StoreError> {
        let (unlocked, locked): (Vec<OwnedObjectPath>, Vec<OwnedObjectPath>) = self
            .proxy(ROOT, INTERFACE)?
            .call("SearchItems", &(Self::attributes(),))
            .map_err(|_| StoreError::Unavailable)?;
        if unlocked.len() + locked.len() > 1 {
            return Err(StoreError::Ambiguous);
        }
        if !locked.is_empty() {
            return Err(StoreError::Locked);
        }
        Ok(unlocked.into_iter().next())
    }
    fn read(&self) -> Result<Vec<u8>, StoreError> {
        let item = self.locate()?.ok_or(StoreError::Missing)?;
        let proxy = self.proxy(item.as_str(), "org.freedesktop.Secret.Item")?;
        let locked: bool = proxy
            .get_property("Locked")
            .map_err(|_| StoreError::Unavailable)?;
        if locked {
            return Err(StoreError::Locked);
        }
        let (session, parameters, bytes, _content): Secret = proxy
            .call("GetSecret", &(&self.session,))
            .map_err(|_| StoreError::Unavailable)?;
        // Providers can replace MIME metadata. Only the canonical bytes define document validity.
        if session != self.session || !parameters.is_empty() {
            return Err(StoreError::InvalidDocument);
        }
        decode(&bytes)?;
        Ok(bytes)
    }
    fn commit(&self, bytes: &[u8]) -> Result<Vec<u8>, StoreError> {
        let item = self.locate()?;
        // Refuse corrupt stored documents before issuing any mutation.
        match self.read() {
            Ok(_) | Err(StoreError::Missing) => {}
            Err(e) => return Err(e),
        }
        let secret: Secret = (
            self.session.clone(),
            Vec::new(),
            bytes.to_vec(),
            "application/octet-stream".into(),
        );
        match item {
            Some(item) => {
                let proxy = self.proxy(item.as_str(), "org.freedesktop.Secret.Item")?;
                let locked: bool = proxy
                    .get_property("Locked")
                    .map_err(|_| StoreError::Unavailable)?;
                if locked {
                    return Err(StoreError::Locked);
                }
                // Once issued, any API error may conceal an accepted mutation.
                proxy
                    .call::<_, _, ()>("SetSecret", &(secret,))
                    .map_err(|_| StoreError::Uncertain)?;
            }
            None => {
                let proxy = self.proxy(
                    self.collection.as_str(),
                    "org.freedesktop.Secret.Collection",
                )?;
                let mut properties = HashMap::new();
                properties.insert(
                    "org.freedesktop.Secret.Item.Label",
                    Value::from("Earph Sendspin committed credentials"),
                );
                properties.insert(
                    "org.freedesktop.Secret.Item.Attributes",
                    Value::from(Self::attributes()),
                );
                let (item, prompt): (OwnedObjectPath, OwnedObjectPath) = proxy
                    .call("CreateItem", &(properties, secret, false))
                    .map_err(|_| StoreError::Uncertain)?;
                // Disconnect invalidates a returned prompt. Never invoke Prompt or assume dismissal proves completion.
                if item.as_str() == "/" || prompt.as_str() != "/" {
                    return Err(StoreError::Uncertain);
                }
            }
        }
        let readback = self.read().map_err(|_| StoreError::Uncertain)?;
        if readback != bytes {
            return Err(StoreError::Uncertain);
        }
        Ok(readback)
    }
}
pub(crate) fn execute(address: &str, action: &Action) -> Result<Vec<u8>, StoreError> {
    let store = Store::open(address)?;
    match action {
        Action::Read => store.read(),
        Action::Commit(bytes) => store.commit(bytes),
    }
    // Connection teardown closes the session and invalidates any uninvoked prompt.
}
