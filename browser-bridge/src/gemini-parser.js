(function installGeminiParser(root) {
  function firstJsonArray(text, start) {
    let depth = 0;
    let inString = false;
    let escaped = false;
    for (let index = start; index < text.length; index += 1) {
      const char = text[index];
      if (escaped) {
        escaped = false;
        continue;
      }
      if (inString && char === '\\') {
        escaped = true;
        continue;
      }
      if (char === '"') {
        inString = !inString;
        continue;
      }
      if (inString) continue;
      if (char === '[') depth += 1;
      if (char === ']' && --depth === 0) return text.substring(start, index + 1);
    }
    return null;
  }

  function parseLimits(text) {
    if (!text.includes('jSf9Qc')) return null;
    const start = text.indexOf('[');
    if (start < 0) return null;

    try {
      const block = firstJsonArray(text, start);
      if (!block) return null;
      const outer = JSON.parse(block);
      const inner = JSON.parse(outer[0][2]);
      const limits = inner[1];
      if (!Array.isArray(limits)) return null;

      const result = { tier: inner[0] };
      for (const limit of limits) {
        if (limit[2] === 1) {
          result.remaining5h = limit[0];
          result.ratio5h = limit[1];
          result.resetTime5h = limit[3]?.[0]?.[0];
        } else if (limit[2] === 2) {
          result.remaining7d = limit[0];
          result.ratio7d = limit[1];
          result.resetTime7d = limit[3]?.[0]?.[0];
        }
      }
      return result.ratio5h != null && result.ratio7d != null ? result : null;
    } catch (error) {
      return null;
    }
  }

  root.AiQuotaDeckGeminiParser = Object.freeze({ firstJsonArray, parseLimits });
})(globalThis);
