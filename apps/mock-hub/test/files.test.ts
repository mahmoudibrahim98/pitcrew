import assert from 'node:assert/strict';
import { test } from 'node:test';
import { AGENT, DEVICE, ID, call, withServer } from './helpers.ts';
import { files } from '../src/files.ts';

test('files enforce path tables before access and authorize before parsing', async () => {
  await withServer(async server => {
    const route = `/v1/workstreams/${ID.submission}/files/content`;
    assert.equal((await call(server, 'PUT', `${route}?loc=0&path=file`, { token: AGENT, raw: '{' })).status, 403);
    for (const path of ['../x','/absolute','.','a//b','a/./b','a/../b','a/','a\\b','a\0b']) {
      assert.equal((await call(server, 'GET', `${route}?${new URLSearchParams({loc:'0',path})}`, { token: DEVICE })).status, 400, path);
    }
    if (process.platform === 'win32') for (const path of ['C:/file','C:file','file:stream','CON.txt','COM¹','LPT².txt','end.','end ']) {
      assert.equal((await call(server, 'GET', `${route}?${new URLSearchParams({loc:'0',path})}`, { token: DEVICE })).status, 400, path);
    }
  });
});
test('in-memory trees share a location and cap sorted entries', async () => {
  await withServer(async server => {
    const stream = server.hub.findWorkstream(ID.submission);
    assert.ok(stream);
    // The test never opens the synthetic location paths in the fixture.
    for (let n = 5001; n >= 0; n--) {
      const reply = files(server.hub, stream.id, new URLSearchParams({loc:'0', path:`entry-${n.toString().padStart(5,'0')}`}), {revision:null, encoding:'utf8',content:'synthetic'}, 'write');
      assert.equal(reply.status, 200);
    }
    const reply = files(server.hub, stream.id, new URLSearchParams({loc:'0',path:''}), undefined, 'list');
    const body = reply.body as { entries: { name: string }[]; truncated: boolean };
    assert.equal(body.entries.length, 5000); assert.equal(body.truncated, true);
    assert.deepEqual(body.entries.map(e => e.name), [...body.entries.map(e => e.name)].sort());
    const other = {...stream, id:'01J00000000000000000000123'};
    server.hub.workstreams.push(other);
    const read = files(server.hub, other.id, new URLSearchParams({loc:'0',path:'entry-00000'}), undefined, 'read');
    assert.equal((read.body as {content:string}).content, 'synthetic');
  });
});
