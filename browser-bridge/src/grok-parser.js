(function installGrokParser(root) {
  const USAGE_PERCENT_FIELD = 1;
  const PERIOD_END_FIELD = 5;
  const PRODUCT_USAGE_FIELD = 7;
  const PRODUCT_LABELS = {
    2: 'Grok Build',
    4: 'Chat',
    5: 'Imagine',
    6: 'Voice'
  };

  function decodeProto(bytes) {
    const fields = {};
    let offset = 0;

    function varint() {
      let value = 0;
      let scale = 1;
      while (offset < bytes.length) {
        const byte = bytes[offset++];
        value += (byte & 0x7f) * scale;
        if (!(byte & 0x80)) break;
        scale *= 128;
      }
      return value;
    }

    while (offset < bytes.length) {
      const key = varint();
      const field = Math.floor(key / 8);
      const wire = key & 7;
      if (!field) break;

      if (wire === 0) {
        (fields[field] || (fields[field] = [])).push(varint());
      } else if (wire === 2) {
        const length = varint();
        if (offset + length > bytes.length) break;
        (fields[field] || (fields[field] = [])).push(bytes.subarray(offset, offset + length));
        offset += length;
      } else if (wire === 5) {
        if (offset + 4 > bytes.length) break;
        const value = new DataView(bytes.buffer, bytes.byteOffset + offset, 4).getFloat32(0, true);
        (fields[field] || (fields[field] = [])).push(value);
        offset += 4;
      } else if (wire === 1) {
        offset += 8;
      } else {
        break;
      }
    }
    return fields;
  }

  function first(fields, number) {
    const values = fields[number];
    return values?.length ? values[0] : undefined;
  }

  function firstGrpcDataFrame(buffer) {
    const bytes = new Uint8Array(buffer);
    let offset = 0;
    while (offset + 5 <= bytes.length) {
      const flag = bytes[offset];
      const length = new DataView(bytes.buffer, bytes.byteOffset + offset + 1, 4)
        .getUint32(0, false);
      const end = offset + 5 + length;
      if (end > bytes.length) return null;
      if ((flag & 0x80) === 0) return decodeProto(bytes.subarray(offset + 5, end));
      offset = end;
    }
    return null;
  }

  function readPaidUsage(response) {
    const configBytes = first(response, 1);
    if (!configBytes) return null;
    const config = decodeProto(configBytes);

    const usedRaw = first(config, USAGE_PERCENT_FIELD);
    const used = Number.isFinite(usedRaw) ? Math.max(0, Math.min(100, usedRaw)) : 0;

    let resetAt = null;
    const periodEnd = first(config, PERIOD_END_FIELD);
    if (periodEnd) {
      const seconds = first(decodeProto(periodEnd), 1);
      if (Number.isFinite(seconds) && seconds > 0) resetAt = seconds * 1000;
    }

    const products = (config[PRODUCT_USAGE_FIELD] || [])
      .map((entry) => {
        const product = decodeProto(entry);
        const id = first(product, 1);
        const percent = first(product, 2);
        return {
          id,
          label: PRODUCT_LABELS[id] || 'Other',
          percent: Number.isFinite(percent) ? percent : 0
        };
      })
      .filter((product) => product.percent > 0)
      .sort((a, b) => b.percent - a.percent);

    return { used, resetAt, products };
  }

  root.AiQuotaDeckGrokParser = Object.freeze({
    decodeProto,
    firstGrpcDataFrame,
    readPaidUsage
  });
})(globalThis);
