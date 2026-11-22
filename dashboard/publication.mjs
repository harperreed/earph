// ABOUTME: Verifies the authenticated owner and private audience before progress publication.
// ABOUTME: Keeps identity checks separate from network operations for direct behavioral tests.
export function verifyMetadata(metadata, identity, slug) {
  const account = identity.account ?? identity.login;
  const capabilities = metadata.capabilities;
  if (typeof account !== 'string' || !account || identity.admin_view === true
      || metadata.slug !== slug || metadata.kind !== 'page'
      || metadata.owner !== account || metadata.visibility !== 'private'
      || capabilities?.role !== 'owner' || capabilities.source !== 'ownership'
      || capabilities.publish !== true || capabilities.write_data !== true) {
    throw new Error('Artifact ownership, kind, or private visibility does not match. No further write is allowed.');
  }
}
