const assert = require('node:assert/strict');
const C = require('../ui/core');

const rows = [
  { path: 'a', caption: 'caption', status: 'approved' },
  { path: 'b', caption: 'caption', status: 'rejected' },
  { path: 'c', caption: '', status: 'pending' },
  { path: 'd', caption: 'partial', status: 'failed' },
];
assert.deepEqual(C.exportable(rows, 'approved').map(i => i.path), ['a']);
assert.deepEqual(C.exportable(rows, 'captioned').map(i => i.path), ['a']);
assert.deepEqual(C.eligible(rows, 'selected', new Set(['c'])).map(i => i.path), ['c']);
assert.equal(C.eligible(rows, 'selected', new Set()).length, 0);
assert.equal(C.eligible(rows, 'failed', new Set())[0].path, 'd');
assert.equal(C.restore([{ status: 'running' }])[0].status, 'pending');
assert.equal(C.merge(rows, [{ path: 'a', caption: 'changed' }, { path: 'e', caption: 'existing' }]).length, 5);
assert.equal(C.merge([], [{ path: 'e', caption: 'existing' }])[0].status, 'review');

// Halogen pixel budget defaults & clamping (3.69 MP default, 14.75 MP refusal ceiling)
assert.equal(C.HALOGEN_DEFAULT_BUDGET, 3686400);
assert.equal(C.HALOGEN_MAX_BUDGET, 14745600);
assert.equal(
  C.estimateBudget([
    { pixels: 1e6, tokens: 1000 },
    { pixels: 2e6, tokens: 2000 },
    { pixels: 11560000, tokens: 4000 },
  ]).pixels,
  4000000
);
assert.equal(
  C.estimateBudget([
    { pixels: 1e6, tokens: 1000 },
    { pixels: 2e6, tokens: 2000 },
    { pixels: 16000000, tokens: 16000 },
  ]).pixels,
  14745600
);
assert.equal(
  C.estimateBudget([
    { pixels: 1e6, tokens: 256 },
    { pixels: 2e6, tokens: 256 },
    { pixels: 11560000, tokens: 256 },
  ]).pixels,
  3686400
);
console.log('Collection tests passed: filtering, selection, recovery, deduplication, and Halogen image budgets.');

// Prompt layout with and without reference images
const refCfg = {
  instruction: 'Describe the target.',
  referenceInstruction: 'Use references for guidance.',
  references: [
    { url: 'reference-a', note: 'Hand anatomy' },
    { url: 'reference-b', note: '' },
  ],
};
const first = C.captionMessages(refCfg, 'target-a');
const second = C.captionMessages(refCfg, 'target-b');
assert.deepEqual(first.slice(0, -1), C.prefixMessages(refCfg));
assert.deepEqual(first.slice(0, -1), second.slice(0, -1));
assert.equal(first.at(-1).role, 'user');
assert.deepEqual(
  first[0].content.filter(x => x.type === 'image_url').map(x => x.image_url.url),
  ['reference-a', 'reference-b']
);
assert.equal(first.at(-1).content.at(-1).image_url.url, 'target-a');
assert(!JSON.stringify(second).includes('target-a'));

const plainCfg = { instruction: 'Caption cleanly.' };
const plainMsgs = C.captionMessages(plainCfg, 'target-1');
assert.equal(plainMsgs.length, 2);
assert.deepEqual(plainMsgs.slice(0, -1), C.prefixMessages(plainCfg));
assert.equal(plainMsgs[0].role, 'system');
assert.equal(plainMsgs[1].role, 'user');
assert.equal(plainMsgs[1].content[0].image_url.url, 'target-1');
console.log('Halogen prompt layout tests passed: shared prefix, last_user_start target separation, and isolation.');

// Sampling, thinking, and prompt_cache_policy payload tests
const greedyPayload = C.buildChatPayload(
  {
    model: 'halogen-qwen3.8-flash-next',
    instruction: 'Caption',
    maxTokens: 4096,
    effort: 'none',
    maxThinkingTokens: 512,
    temperature: 0,
    topP: 0.95,
    topK: 20,
    presencePenalty: 1.5,
    repetitionPenalty: 1.1,
  },
  'data:image/png;base64,abc',
  { noStore: true }
);
assert.equal(greedyPayload.return_progress, true);
assert.equal(greedyPayload.max_completion_tokens, 4096);
assert.equal(greedyPayload.temperature, 0);
assert.equal(greedyPayload.top_p, undefined);
assert.equal(greedyPayload.top_k, undefined);
assert.equal(greedyPayload.presence_penalty, undefined);
assert.equal(greedyPayload.repetition_penalty, undefined);
assert.equal(greedyPayload.reasoning_effort, 'none');
assert.deepEqual(greedyPayload.chat_template_kwargs, { enable_thinking: false });
assert.equal(greedyPayload.max_thinking_tokens, undefined);
assert.equal(greedyPayload.prompt_cache_policy, 'no_store');

const sampledPayload = C.buildChatPayload(
  {
    model: 'halogen-qwen3.8-flash-next',
    instruction: 'Caption',
    maxTokens: 8192,
    effort: 'low',
    maxThinkingTokens: 1024,
    ...C.SAMPLING_PRESETS['qwen-non-thinking'],
  },
  'data:image/png;base64,abc',
  { noStore: false }
);
assert.equal(sampledPayload.temperature, 0.7);
assert.equal(sampledPayload.top_p, 0.8);
assert.equal(sampledPayload.top_k, 20);
assert.equal(sampledPayload.presence_penalty, 1.5);
assert.equal(sampledPayload.repetition_penalty, 1.0);
assert.equal(sampledPayload.reasoning_effort, 'low');
assert.equal(sampledPayload.max_thinking_tokens, 1024);
assert.equal(sampledPayload.prompt_cache_policy, undefined);

// Telemetry formatting & error detection
const formatted = C.formatStats(
  { prompt_n: 200, cache_n: 1800, predicted_n: 90, predicted_per_second: 84.3, draft_n: 50, draft_n_accepted: 40 },
  { prompt_tokens: 2000, completion_tokens: 90, completion_tokens_details: { reasoning_tokens: 120, reasoning_closed_by: 'answer_room' } },
  'used'
);
assert(formatted.includes('2,000 tok'));
assert(formatted.includes('1,800 cached, 90%'));
assert(formatted.includes('84.3 tok/s'));
assert(formatted.includes('MTP 40/50 (80%)'));
assert(formatted.includes('thinking 120 tok'));
assert(formatted.includes('closed by answer_room'));
assert(formatted.includes('X-Halogen-From: used'));

assert(C.serverUnavailable('HTTP 400: HALOGEN_VISION_TOWER is not set'));
assert(C.serverUnavailable('HTTP 503: engine_busy'));
assert(C.serverUnavailable('HTTP 503: engine_stalled'));
assert(C.serverUnavailable('HTTP 503: host_memory'));
assert(!C.serverUnavailable('Model returned no caption.'));
console.log('Halogen payload, telemetry, and error detection tests passed.');
