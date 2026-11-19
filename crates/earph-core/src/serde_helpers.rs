// ABOUTME: Requires explicitly present nullable fields in shared Core contracts.
// ABOUTME: Keeps missing fields distinct from a producer's explicit null observation.
use serde::{Deserialize, Deserializer};

pub(crate) fn nullable<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    Option::deserialize(deserializer)
}
