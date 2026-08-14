const assert = require('node:assert/strict');
const crypto = require('node:crypto');
const manifest = require('../manifest.json');

require('../src/gemini-parser.js');
require('../src/grok-parser.js');

function extensionId(publicKey) {
  const digest = crypto.createHash('sha256').update(Buffer.from(publicKey, 'base64')).digest();
  return [...digest.subarray(0, 16)]
    .map((byte) => String.fromCharCode(97 + (byte >> 4), 97 + (byte & 15)))
    .join('');
}

assert.equal(extensionId(manifest.key), 'alckoeangnmpomfnafaajjbpniomhnke');

const inner = [2, [
  [1200, 0.5, 1, [[1800018000, 0]]],
  [24000, 0.25, 2, [[1800604800, 0]]]
]];
const response = `)]}'\n${JSON.stringify([
  ['wrb.fr', 'jSf9Qc', JSON.stringify(inner), null]
])}\n25\n[["e",4,null,null,284]]`;
assert.deepEqual(AiQuotaDeckGeminiParser.parseLimits(response), {
  tier: 2,
  remaining5h: 1200,
  ratio5h: 0.5,
  resetTime5h: 1800018000,
  remaining7d: 24000,
  ratio7d: 0.25,
  resetTime7d: 1800604800
});
assert.equal(AiQuotaDeckGeminiParser.parseLimits('not quota data'), null);

function varint(value) {
  const bytes = [];
  while (value > 127) {
    bytes.push((value % 128) | 0x80);
    value = Math.floor(value / 128);
  }
  bytes.push(value);
  return bytes;
}

const tag = (field, wire) => varint(field * 8 + wire);
const varintField = (field, value) => tag(field, 0).concat(varint(value));
const lenField = (field, bytes) => tag(field, 2).concat(varint(bytes.length), bytes);

function floatField(field, value) {
  const buffer = new ArrayBuffer(4);
  new DataView(buffer).setFloat32(0, value, true);
  return tag(field, 5).concat([...new Uint8Array(buffer)]);
}

const chat = varintField(1, 4).concat(floatField(2, 30));
const imagine = varintField(1, 5).concat(floatField(2, 12.5));
const config = floatField(1, 42.5)
  .concat(lenField(5, varintField(1, 1800604800)))
  .concat(lenField(7, chat), lenField(7, imagine));
const message = lenField(1, config);
const header = [0, 0, 0, 0, message.length];
const trailer = [...Buffer.from('grpc-status:0\r\n')];
const frames = new Uint8Array(header.concat(message, [0x80, 0, 0, 0, trailer.length], trailer));

const decodedFrame = AiQuotaDeckGrokParser.firstGrpcDataFrame(frames.buffer);
assert.deepEqual(AiQuotaDeckGrokParser.readPaidUsage(decodedFrame), {
  used: 42.5,
  resetAt: 1800604800000,
  products: [
    { id: 4, label: 'Chat', percent: 30 },
    { id: 5, label: 'Imagine', percent: 12.5 }
  ]
});

// A config whose known fields all fail to decode is schema drift, not 0% usage:
// pushing a fabricated 0% would overwrite a real snapshot in the on-disk cache.
const untouched = AiQuotaDeckGrokParser.decodeProto(new Uint8Array(lenField(1, [])));
assert.equal(AiQuotaDeckGrokParser.readPaidUsage(untouched), null);

// But a genuine 0% is real: proto3 omits zero-valued scalars, so an untouched
// account sends no usage field while the period end still decodes.
const zeroUsage = AiQuotaDeckGrokParser.decodeProto(
  new Uint8Array(lenField(1, lenField(5, varintField(1, 1800604800))))
);
assert.deepEqual(AiQuotaDeckGrokParser.readPaidUsage(zeroUsage), {
  used: 0,
  resetAt: 1800604800000,
  products: []
});

// Product entries that decode are themselves proof the schema holds, even when
// every percent is 0 (and so gets filtered from the breakdown).
const zeroProducts = AiQuotaDeckGrokParser.decodeProto(
  new Uint8Array(lenField(1, lenField(7, varintField(1, 4))))
);
assert.deepEqual(AiQuotaDeckGrokParser.readPaidUsage(zeroProducts), {
  used: 0,
  resetAt: null,
  products: []
});

console.log('stable extension id and Gemini/Grok parsers: ok');
