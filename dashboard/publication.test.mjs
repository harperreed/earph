// ABOUTME: Checks publication ownership against canonical accounts and linked logins.
// ABOUTME: Keeps foreign accounts, nonowners and nonprivate artifacts outside the publisher guard.
import test from 'node:test';
import assert from 'node:assert/strict';
import { verifyMetadata } from './publication.mjs';

const slug = 'earph-progress';
const identity = {login:'linked@example.com',account:'owner@example.com',admin_view:false};
const metadata = {
  slug,kind:'page',owner:identity.account,visibility:'private',
  capabilities:{role:'owner',source:'ownership',publish:true,write_data:true},
};

test('linked login can publish its canonical account artifact', () => {
  assert.doesNotThrow(() => verifyMetadata(metadata,identity,slug));
});

test('unlinked login retains its own publication authority', () => {
  assert.doesNotThrow(() => verifyMetadata(metadata,{login:metadata.owner},slug));
});

test('foreign canonical account cannot borrow matching login ownership', () => {
  assert.throws(() => verifyMetadata(metadata,{login:metadata.owner,account:'foreign@example.com'},slug));
});

test('private audience and direct owner capabilities remain required', () => {
  const owner = {login:metadata.owner,account:metadata.owner,admin_view:false};
  for (const change of [
    {visibility:'everyone'}, {kind:'doc'}, {slug:'another-artifact'},
    {capabilities:{...metadata.capabilities,role:'editor'}},
    {capabilities:{...metadata.capabilities,source:'admin'}},
    {capabilities:{...metadata.capabilities,publish:false}},
    {capabilities:{...metadata.capabilities,write_data:false}},
  ]) assert.throws(() => verifyMetadata({...metadata,...change},owner,slug));
  assert.throws(() => verifyMetadata(metadata,{...owner,admin_view:true},slug));
  assert.throws(() => verifyMetadata(metadata,{...owner,account:''},slug));
});
