/* Pure collection, prompt-cache, and halogen-flash-server payload rules shared with tests. */
const Collection = (() => {
  const HALOGEN_DEFAULT_BUDGET = 3686400; // 2560 x 1440 (HALOGEN_VISION_MAX_PIXELS default)
  const HALOGEN_MAX_BUDGET = 14745600;    // 4 x 3,686,400 (server refusal limit)

  const SAMPLING_PRESETS = {
    greedy: { temperature: 0, topP: '', topK: '', presencePenalty: '', repetitionPenalty: '' },
    'qwen-non-thinking': { temperature: 0.7, topP: 0.8, topK: 20, presencePenalty: 1.5, repetitionPenalty: 1.0 },
    'qwen-thinking': { temperature: 1.0, topP: 0.95, topK: 20, presencePenalty: 0, repetitionPenalty: 1.0 },
  };

  const anatomyPrompt = `Write a concise, factual natural-language caption for this image, for training a realistic anatomy image-generation LoRA. Describe the visible subject, body parts, pose, viewing angle, framing, contact with objects, clothing or exposed skin, and relevant lighting or background. Describe spatial relationships clearly. Only state details supported by the image. Do not guess hidden body parts, finger counts, or left/right orientation. Do not use generic quality claims such as "perfect anatomy". Do not give instructions or anatomical evaluations. Return only the caption, without headings, quotation marks, tags, or commentary.`;

  function eligible(items, scope, selected) {
    return items.filter(i =>
      scope === 'regenerate-selected'
        ? selected.has(i.path) && i.status !== 'running'
        : scope === 'failed'
          ? i.status === 'failed'
          : i.status === 'pending' && (scope !== 'selected' || selected.has(i.path))
    );
  }

  function exportable(items, scope) {
    return items.filter(i => i.caption.trim() && (scope === 'approved' ? i.status === 'approved' : ['approved', 'review'].includes(i.status)));
  }

  function restore(items) {
    return items.map(i => ({ ...i, status: i.status === 'running' ? 'pending' : i.status }));
  }

  function merge(items, additions) {
    const paths = new Set(items.map(i => i.path));
    return [
      ...items,
      ...additions
        .filter(i => {
          if (paths.has(i.path)) return false;
          paths.add(i.path);
          return true;
        })
        .map(i => ({ ...i, status: i.caption.trim() ? 'review' : 'pending', error: i.warning || '' })),
    ];
  }

  function estimateBudget(observations, fallback = HALOGEN_DEFAULT_BUDGET) {
    const [a, b, c] = observations;
    const near = (x, y, t) => Math.abs(x - y) <= t * Math.max(Math.abs(x), Math.abs(y));
    if (a.tokens > 0 && near(b.tokens / a.tokens, b.pixels / a.pixels, 0.2)) {
      const cap = Math.min(HALOGEN_MAX_BUDGET, c.pixels);
      const pixels = Math.round(Math.min(cap, Math.max(65536, (c.tokens * b.pixels) / b.tokens)));
      return {
        pixels,
        description: `Measured image budget: ${(pixels / 1000000).toFixed(2)} MP${pixels >= cap * 0.95 ? ' (no cap detected up to this size)' : ''}.`,
      };
    }
    if (a.tokens > 0 && near(a.tokens, b.tokens, 0.03) && near(a.tokens, c.tokens, 0.03)) {
      return {
        pixels: fallback,
        description: 'Fixed-resolution model: token count does not reveal its input size. Keeping the current preparation budget.',
      };
    }
    throw Error('Unexpected image token counts; use your server’s documented image budget.');
  }

  function serverUnavailable(message) {
    return /error sending request|error decoding response body|connection (?:refused|reset|closed|failed)|failed to (?:read|connect)|broken pipe|no response for five minutes|http (?:502|503|504)\b|non-causal attention requires|ggml_assert|engine_busy|engine_stalled|host_memory|HALOGEN_VISION_TOWER/i.test(
      message
    );
  }

  function prefixMessages(config) {
    if (config.references?.length) {
      const content = [{ type: 'text', text: config.instruction }];
      if (config.referenceInstruction != null && String(config.referenceInstruction).trim()) {
        content.push({ type: 'text', text: config.referenceInstruction });
      }
      config.references.forEach((ref, i) => {
        content.push(
          { type: 'text', text: `REFERENCE ${i + 1}${ref.note ? ' — ' + ref.note : ''}` },
          { type: 'image_url', image_url: { url: ref.url } }
        );
      });
      content.push({ type: 'text', text: 'END OF REFERENCES. Wait for the target image and return only its caption.' });
      return [
        { role: 'user', content },
        { role: 'assistant', content: 'Ready. Send the target image and I will return only its caption.' },
      ];
    }
    return [{ role: 'system', content: config.instruction }];
  }

  function captionMessages(config, targetUrl) {
    if (config.references?.length) {
      return [
        ...prefixMessages(config),
        {
          role: 'user',
          content: [
            { type: 'text', text: 'TARGET IMAGE — return only the caption.' },
            { type: 'image_url', image_url: { url: targetUrl } },
          ],
        },
      ];
    }
    return [
      { role: 'system', content: config.instruction },
      { role: 'user', content: [{ type: 'image_url', image_url: { url: targetUrl } }] },
    ];
  }

  function thinkingFields(effort, maxThinkingTokens) {
    if (effort === 'none' || effort === 'none-qwen') {
      return { reasoning_effort: 'none', chat_template_kwargs: { enable_thinking: false } };
    }
    const out = {};
    if (effort) {
      out.reasoning_effort = effort;
      out.chat_template_kwargs = { reasoning_effort: effort };
    }
    const cap = Number(maxThinkingTokens);
    if (Number.isInteger(cap) && cap > 0) {
      out.max_thinking_tokens = cap;
    }
    return out;
  }

  function optNum(value) {
    if (value === '' || value == null) return null;
    const n = Number(value);
    return Number.isFinite(n) ? n : null;
  }

  function samplingFields(config) {
    const temp = Number(config.temperature);
    const out = { temperature: temp };
    // halogen-flash-server rejects non-default penalties and filters on greedy (temperature: 0) with HTTP 400.
    if (temp > 0) {
      const topP = optNum(config.topP);
      const topK = optNum(config.topK);
      const presence = optNum(config.presencePenalty);
      const repetition = optNum(config.repetitionPenalty);
      if (topP != null) out.top_p = topP;
      if (topK != null) out.top_k = topK;
      if (presence != null) out.presence_penalty = presence;
      if (repetition != null) out.repetition_penalty = repetition;
    }
    return out;
  }

  function buildChatPayload(config, targetUrl, options = {}) {
    return {
      model: config.model,
      stream: true,
      return_progress: true,
      max_completion_tokens: config.maxTokens,
      ...samplingFields(config),
      ...thinkingFields(config.effort, config.maxThinkingTokens),
      ...(options.noStore ? { prompt_cache_policy: 'no_store' } : {}),
      messages: captionMessages(config, targetUrl),
    };
  }

  function formatStats(stats, usage, halogenFrom) {
    const parts = [];
    const n = v => (v == null || v === '' || !Number.isFinite(Number(v)) ? null : Number(v));
    const cacheN = n(stats?.cache_n ?? usage?.prompt_tokens_details?.cached_tokens);
    const promptN = n(stats?.prompt_n);
    const totalPrompt = n(usage?.prompt_tokens) ?? (promptN != null || cacheN != null ? (promptN ?? 0) + (cacheN ?? 0) : null);
    if (totalPrompt != null && totalPrompt > 0) {
      const cached = cacheN ?? 0;
      if (cached > 0) {
        const pct = Math.min(100, Math.round((cached / totalPrompt) * 100));
        parts.push(`prefill ${totalPrompt.toLocaleString()} tok (${cached.toLocaleString()} cached, ${pct}%)`);
      } else {
        parts.push(`prefill ${totalPrompt.toLocaleString()} tok`);
      }
    }
    const predN = n(stats?.predicted_n ?? usage?.completion_tokens);
    const tps = n(stats?.predicted_per_second);
    if (tps != null) {
      parts.push(predN != null ? `decode ${predN.toLocaleString()} tok @ ${tps.toFixed(1)} tok/s` : `decode ${tps.toFixed(1)} tok/s`);
    } else if (predN != null) {
      parts.push(`decode ${predN.toLocaleString()} tok`);
    }
    const draftN = n(stats?.draft_n);
    const draftAcc = n(stats?.draft_n_accepted);
    if (draftN != null && draftN > 0 && draftAcc != null && draftAcc <= draftN) {
      const pct = Math.round((draftAcc / draftN) * 100);
      parts.push(`MTP ${draftAcc}/${draftN} (${pct}%)`);
    }
    const details = usage?.completion_tokens_details;
    const reasonTok = n(details?.reasoning_tokens);
    if (reasonTok != null && reasonTok > 0) {
      parts.push(`thinking ${reasonTok.toLocaleString()} tok`);
    }
    if (details?.reasoning_closed_by) {
      parts.push(`closed by ${details.reasoning_closed_by}`);
    }
    if (halogenFrom) {
      parts.push(`X-Halogen-From: ${halogenFrom}`);
    }
    return parts.join(' · ');
  }

  return {
    HALOGEN_DEFAULT_BUDGET,
    HALOGEN_MAX_BUDGET,
    SAMPLING_PRESETS,
    anatomyPrompt,
    eligible,
    exportable,
    restore,
    merge,
    estimateBudget,
    serverUnavailable,
    prefixMessages,
    captionMessages,
    thinkingFields,
    samplingFields,
    buildChatPayload,
    formatStats,
  };
})();
if (typeof module !== 'undefined') module.exports = Collection;
