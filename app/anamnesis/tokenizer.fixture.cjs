// Exact synthetic vocabulary for loader/packing tests, NOT any model tokenizer.
// A production operator supplies a self-contained exact encoder in this ABI.
module.exports.createEncoder = function (assets) {
  const vocabulary = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(assets.vocabulary));
  if (!Array.isArray(vocabulary) || vocabulary.some(token => typeof token !== 'string' || !token)) throw new Error('invalid vocabulary');
  const encoder = new TextEncoder();
  const merges = vocabulary.map(token => encoder.encode(token)).sort((a, b) => b.length - a.length);
  return function encode(text) {
    const bytes = encoder.encode(text);
    if (bytes.length > 1048576) throw new Error('unbounded tokenizer input');
    let count = 0;
    for (let i = 0; i < bytes.length; count++) {
      const token = merges.find(token => token.every((byte, j) => bytes[i + j] === byte));
      i += token ? token.length : 1; // Vocabulary contains all 256 single-byte tokens.
    }
    return count;
  };
};
