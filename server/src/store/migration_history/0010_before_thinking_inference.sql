CREATE TABLE model_configs_new (
    model_hash TEXT PRIMARY KEY,
    config_hash TEXT NOT NULL UNIQUE,
    sort_order INTEGER NOT NULL DEFAULT 0,
    display_name TEXT NOT NULL,
    group_name TEXT,
    model_type TEXT NOT NULL CHECK(model_type IN ('openai', 'anthropic')),
    base_url TEXT NOT NULL,
    use_full_url INTEGER NOT NULL DEFAULT 0 CHECK(use_full_url IN (0, 1)),
    api_key TEXT NOT NULL,
    tooltip_data TEXT NOT NULL,
    model_id TEXT NOT NULL,
    supports_thinking INTEGER NOT NULL DEFAULT 0 CHECK(supports_thinking IN (0, 1)),
    supports_images INTEGER NOT NULL DEFAULT 0 CHECK(supports_images IN (0, 1)),
    supports_fast INTEGER NOT NULL DEFAULT 0 CHECK(supports_fast IN (0, 1)),
    reasoning_effort TEXT,
    openai_endpoint TEXT NOT NULL DEFAULT '',
    openai_extra_params_enabled INTEGER NOT NULL DEFAULT 0 CHECK(openai_extra_params_enabled IN (0, 1)),
    openai_extra_params_json TEXT NOT NULL DEFAULT '{}',
    custom_headers_enabled INTEGER NOT NULL DEFAULT 0 CHECK(custom_headers_enabled IN (0, 1)),
    custom_headers_json TEXT NOT NULL DEFAULT '{}',
    anthropic_extra_params_enabled INTEGER NOT NULL DEFAULT 0 CHECK(anthropic_extra_params_enabled IN (0, 1)),
    anthropic_extra_params_json TEXT NOT NULL DEFAULT '{}',
    context_window_tokens INTEGER,
    max_completion_tokens INTEGER,
    anthropic_max_tokens INTEGER,
    anthropic_thinking_effort TEXT,
    thinking_budget_tokens INTEGER,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);

INSERT INTO model_configs_new (
    model_hash, config_hash, sort_order, display_name, group_name, model_type, base_url,
    use_full_url, api_key, tooltip_data, model_id, supports_thinking, supports_images,
    supports_fast, reasoning_effort, openai_endpoint, openai_extra_params_enabled,
    openai_extra_params_json, custom_headers_enabled, custom_headers_json,
    anthropic_extra_params_enabled, anthropic_extra_params_json, context_window_tokens,
    max_completion_tokens, anthropic_max_tokens, anthropic_thinking_effort,
    thinking_budget_tokens, created_at_ms, updated_at_ms
)
SELECT
    'byok:' || model_hash, model_hash, sort_order, display_name, group_name, model_type,
    base_url, use_full_url, api_key, tooltip_data, model_id, 0, 0, 0,
    reasoning_effort, openai_endpoint, openai_extra_params_enabled,
    openai_extra_params_json, custom_headers_enabled, custom_headers_json,
    anthropic_extra_params_enabled, anthropic_extra_params_json, context_window_tokens,
    max_completion_tokens, anthropic_max_tokens, anthropic_thinking_effort,
    thinking_budget_tokens, created_at_ms, updated_at_ms
FROM model_configs;

UPDATE llm_calls
SET model_hash = 'byok:' || model_hash
WHERE model_hash IS NOT NULL
  AND model_hash NOT LIKE 'plugin:%'
  AND model_hash NOT LIKE 'byok:%';

UPDATE service_settings
SET value_json = json_set(
    value_json,
    '$.model_id',
    'byok:' || json_extract(value_json, '$.model_id')
)
WHERE setting_key = 'commit_settings'
  AND json_valid(value_json)
  AND json_type(value_json, '$.model_id') = 'text'
  AND EXISTS (
      SELECT 1
      FROM model_configs
      WHERE model_hash = json_extract(value_json, '$.model_id')
  );

DROP TABLE model_configs;
ALTER TABLE model_configs_new RENAME TO model_configs;

CREATE INDEX model_configs_sort
ON model_configs(sort_order, display_name);
