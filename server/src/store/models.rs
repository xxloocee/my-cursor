//! Persists model and provider configuration.
use std::{collections::HashSet, str::FromStr};

use sqlx::{Row, Sqlite, Transaction};
use uuid::Uuid;

use crate::{
    model::{
        model_config_hash, normalize_model_input, ModelConfig, ModelConfigInput, ModelType,
        ModelUpdate, BUILTIN_MODEL_ID_PREFIX,
    },
    Error, Result,
};

use super::{now_ms, settings::COMMIT_SETTINGS_KEY, CommitSettings, Store};

const MODEL_COLUMNS: &str = r#"
    model_hash, config_hash, sort_order, display_name, group_name, model_type, base_url, use_full_url, api_key, tooltip_data,
    model_id, supports_thinking, supports_images, supports_fast, reasoning_effort, openai_endpoint, openai_extra_params_enabled,
    openai_extra_params_json, custom_headers_enabled, custom_headers_json,
    anthropic_extra_params_enabled, anthropic_extra_params_json, context_window_tokens,
    max_completion_tokens, anthropic_max_tokens, anthropic_thinking_effort,
    thinking_budget_tokens, created_at_ms, updated_at_ms
"#;

impl Store {
    pub async fn models(&self) -> Result<Vec<ModelConfig>> {
        let query =
            format!("SELECT {MODEL_COLUMNS} FROM model_configs ORDER BY sort_order, display_name");
        sqlx::query(&query)
            .fetch_all(&self.pool)
            .await?
            .into_iter()
            .map(model_from_row)
            .collect()
    }

    pub async fn model(&self, hash: &str) -> Result<Option<ModelConfig>> {
        let query = format!("SELECT {MODEL_COLUMNS} FROM model_configs WHERE model_hash = ?");
        sqlx::query(&query)
            .bind(hash)
            .fetch_optional(&self.pool)
            .await?
            .map(model_from_row)
            .transpose()
    }

    pub async fn create_model(&self, input: &ModelConfigInput) -> Result<ModelConfig> {
        let mut models = self.create_models(std::slice::from_ref(input)).await?;
        Ok(models.remove(0))
    }

    pub async fn create_models(&self, inputs: &[ModelConfigInput]) -> Result<Vec<ModelConfig>> {
        if inputs.is_empty() {
            return Err(Error::Config("at least one model is required".into()));
        }
        let mut normalized = Vec::with_capacity(inputs.len());
        let mut hashes = HashSet::with_capacity(inputs.len());
        for input in inputs {
            let input = normalize_model_input(input)?;
            let config_hash = model_config_hash(&input)?;
            if !hashes.insert(config_hash.clone()) {
                return Err(Error::Config("model configurations must be unique".into()));
            }
            normalized.push((new_model_id(), config_hash, input));
        }
        let now = now_ms();
        let _write = self.writes.lock().await;
        let mut transaction = self.pool.begin().await?;
        for (model_hash, config_hash, input) in &normalized {
            insert_model(&mut transaction, model_hash, config_hash, input, now).await?;
        }
        transaction.commit().await?;

        let mut saved = Vec::with_capacity(normalized.len());
        for (model_hash, _, _) in normalized {
            saved.push(
                self.model(&model_hash)
                    .await?
                    .expect("inserted model must exist"),
            );
        }
        Ok(saved)
    }

    pub(super) async fn create_models_if_missing(
        &self,
        inputs: &[ModelConfigInput],
    ) -> Result<usize> {
        let mut normalized = Vec::with_capacity(inputs.len());
        let mut hashes = HashSet::with_capacity(inputs.len());
        for input in inputs {
            let input = normalize_model_input(input)?;
            let config_hash = model_config_hash(&input)?;
            if hashes.insert(config_hash.clone()) {
                normalized.push((new_model_id(), config_hash, input));
            }
        }
        let now = now_ms();
        let _write = self.writes.lock().await;
        let mut transaction = self.pool.begin().await?;
        let mut inserted = 0;
        for (model_hash, config_hash, input) in &normalized {
            inserted += usize::from(
                insert_model_with_conflict(
                    &mut transaction,
                    model_hash,
                    config_hash,
                    input,
                    now,
                    true,
                )
                .await?,
            );
        }
        transaction.commit().await?;
        Ok(inserted)
    }

    pub async fn update_model(
        &self,
        current_hash: &str,
        input: &ModelConfigInput,
    ) -> Result<ModelConfig> {
        let updates = [ModelUpdate {
            model_hash: current_hash.to_owned(),
            model: input.clone(),
        }];
        let mut models = self.update_models(&updates).await?;
        Ok(models.remove(0))
    }

    pub async fn update_models(&self, updates: &[ModelUpdate]) -> Result<Vec<ModelConfig>> {
        if updates.is_empty() {
            return Err(Error::Config(
                "at least one model update is required".into(),
            ));
        }
        let mut model_hashes = HashSet::with_capacity(updates.len());
        for update in updates {
            if !model_hashes.insert(update.model_hash.as_str()) {
                return Err(Error::Config(
                    "model update identifiers must be unique".into(),
                ));
            }
        }

        let now = now_ms();
        let _write = self.writes.lock().await;
        let mut transaction = self.pool.begin().await?;
        for update in updates {
            let current = model_in_transaction(&mut transaction, &update.model_hash)
                .await?
                .ok_or_else(|| Error::RunNotFound(format!("model {}", update.model_hash)))?;
            let mut input = update.model.clone();
            if input.api_key.trim().is_empty() {
                input.api_key = current.api_key;
            }
            let input = normalize_model_input(&input)?;
            let config_hash = model_config_hash(&input)?;
            update_model_in_transaction(
                &mut transaction,
                &update.model_hash,
                &config_hash,
                &input,
                now,
            )
            .await?;
        }
        transaction.commit().await?;

        let mut saved = Vec::with_capacity(updates.len());
        for update in updates {
            saved.push(
                self.model(&update.model_hash)
                    .await?
                    .expect("updated model must exist"),
            );
        }
        Ok(saved)
    }

    pub async fn delete_model(&self, hash: &str) -> Result<()> {
        let _write = self.writes.lock().await;
        let mut transaction = self.pool.begin().await?;
        if let Some(value_json) = sqlx::query_scalar::<_, String>(
            "SELECT value_json FROM service_settings WHERE setting_key = ?",
        )
        .bind(COMMIT_SETTINGS_KEY)
        .fetch_optional(&mut *transaction)
        .await?
        {
            let mut settings: CommitSettings = serde_json::from_str(&value_json)?;
            if settings.model_id == hash {
                settings.model_id.clear();
                sqlx::query(
                    "UPDATE service_settings SET value_json = ?, updated_at_ms = ? WHERE setting_key = ?",
                )
                .bind(serde_json::to_string(&settings)?)
                .bind(now_ms())
                .bind(COMMIT_SETTINGS_KEY)
                .execute(&mut *transaction)
                .await?;
            }
        }
        let result = sqlx::query("DELETE FROM model_configs WHERE model_hash = ?")
            .bind(hash)
            .execute(&mut *transaction)
            .await?;
        if result.rows_affected() != 1 {
            return Err(Error::RunNotFound(format!("model {hash}")));
        }
        transaction.commit().await?;
        Ok(())
    }

    pub async fn reorder_models(&self, model_hashes: &[String]) -> Result<Vec<ModelConfig>> {
        let current = self.models().await?;
        let current_hashes = current
            .iter()
            .map(|model| model.model_hash.as_str())
            .collect::<HashSet<_>>();
        let requested_hashes = model_hashes
            .iter()
            .map(String::as_str)
            .collect::<HashSet<_>>();
        if model_hashes.len() != current.len()
            || requested_hashes.len() != current.len()
            || requested_hashes != current_hashes
        {
            return Err(Error::Config(
                "model configuration changed; refresh and try sorting again".into(),
            ));
        }

        let now = now_ms();
        let _write = self.writes.lock().await;
        let mut transaction = self.pool.begin().await?;
        for (index, hash) in model_hashes.iter().enumerate() {
            sqlx::query(
                "UPDATE model_configs SET sort_order = ?, updated_at_ms = ? WHERE model_hash = ?",
            )
            .bind(i64::try_from(index + 1).expect("model order fits in i64"))
            .bind(now)
            .bind(hash)
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;
        self.models().await
    }
}

async fn insert_model(
    transaction: &mut Transaction<'_, Sqlite>,
    model_hash: &str,
    config_hash: &str,
    input: &ModelConfigInput,
    now: i64,
) -> Result<()> {
    insert_model_with_conflict(transaction, model_hash, config_hash, input, now, false).await?;
    Ok(())
}

async fn insert_model_with_conflict(
    transaction: &mut Transaction<'_, Sqlite>,
    model_hash: &str,
    config_hash: &str,
    input: &ModelConfigInput,
    now: i64,
    ignore_existing: bool,
) -> Result<bool> {
    let mut statement = String::from(
        r#"INSERT INTO model_configs(
            model_hash, config_hash, sort_order, display_name, group_name, model_type, base_url, use_full_url, api_key, tooltip_data,
            model_id, supports_thinking, supports_images, supports_fast, reasoning_effort, openai_endpoint, openai_extra_params_enabled,
            openai_extra_params_json, custom_headers_enabled, custom_headers_json,
            anthropic_extra_params_enabled, anthropic_extra_params_json, context_window_tokens,
            max_completion_tokens, anthropic_max_tokens, anthropic_thinking_effort,
            thinking_budget_tokens, created_at_ms, updated_at_ms
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
    );
    if ignore_existing {
        statement.push_str(" ON CONFLICT(config_hash) DO NOTHING");
    }
    let result = sqlx::query(&statement)
        .bind(model_hash)
        .bind(config_hash)
        .bind(input.sort_order)
        .bind(&input.display_name)
        .bind(&input.group_name)
        .bind(input.model_type.as_str())
        .bind(&input.base_url)
        .bind(input.use_full_url)
        .bind(&input.api_key)
        .bind(&input.tooltip_data)
        .bind(&input.model_id)
        .bind(input.supports_thinking)
        .bind(input.supports_images)
        .bind(input.supports_fast)
        .bind(&input.reasoning_effort)
        .bind(&input.openai_endpoint)
        .bind(input.openai_extra_params_enabled)
        .bind(serde_json::to_string(&input.openai_extra_params)?)
        .bind(input.custom_headers_enabled)
        .bind(serde_json::to_string(&input.custom_headers)?)
        .bind(input.anthropic_extra_params_enabled)
        .bind(serde_json::to_string(&input.anthropic_extra_params)?)
        .bind(input.context_window_tokens.map(to_i64).transpose()?)
        .bind(input.max_completion_tokens.map(to_i64).transpose()?)
        .bind(input.anthropic_max_tokens.map(to_i64).transpose()?)
        .bind(&input.anthropic_thinking_effort)
        .bind(input.thinking_budget_tokens.map(to_i64).transpose()?)
        .bind(now)
        .bind(now)
        .execute(&mut **transaction)
        .await?;
    Ok(result.rows_affected() == 1)
}

fn model_from_row(row: sqlx::sqlite::SqliteRow) -> Result<ModelConfig> {
    Ok(ModelConfig {
        model_hash: row.try_get("model_hash")?,
        config_hash: row.try_get("config_hash")?,
        sort_order: row.try_get("sort_order")?,
        display_name: row.try_get("display_name")?,
        group_name: row.try_get("group_name")?,
        model_type: ModelType::from_str(row.try_get("model_type")?)?,
        base_url: row.try_get("base_url")?,
        use_full_url: row.try_get("use_full_url")?,
        api_key: row.try_get("api_key")?,
        tooltip_data: row.try_get("tooltip_data")?,
        model_id: row.try_get("model_id")?,
        supports_thinking: row.try_get("supports_thinking")?,
        supports_images: row.try_get("supports_images")?,
        supports_fast: row.try_get("supports_fast")?,
        reasoning_effort: row.try_get("reasoning_effort")?,
        openai_endpoint: row.try_get("openai_endpoint")?,
        openai_extra_params_enabled: row.try_get("openai_extra_params_enabled")?,
        openai_extra_params: serde_json::from_str(
            row.try_get::<String, _>("openai_extra_params_json")?
                .as_str(),
        )?,
        custom_headers_enabled: row.try_get("custom_headers_enabled")?,
        custom_headers: serde_json::from_str(
            row.try_get::<String, _>("custom_headers_json")?.as_str(),
        )?,
        anthropic_extra_params_enabled: row.try_get("anthropic_extra_params_enabled")?,
        anthropic_extra_params: serde_json::from_str(
            row.try_get::<String, _>("anthropic_extra_params_json")?
                .as_str(),
        )?,
        context_window_tokens: optional_u64(&row, "context_window_tokens")?,
        max_completion_tokens: optional_u64(&row, "max_completion_tokens")?,
        anthropic_max_tokens: optional_u64(&row, "anthropic_max_tokens")?,
        anthropic_thinking_effort: row.try_get("anthropic_thinking_effort")?,
        thinking_budget_tokens: optional_u64(&row, "thinking_budget_tokens")?,
        created_at_ms: row.try_get("created_at_ms")?,
        updated_at_ms: row.try_get("updated_at_ms")?,
    })
}

fn new_model_id() -> String {
    format!("{BUILTIN_MODEL_ID_PREFIX}{}", Uuid::new_v4())
}

async fn model_in_transaction(
    transaction: &mut Transaction<'_, Sqlite>,
    model_hash: &str,
) -> Result<Option<ModelConfig>> {
    let query = format!("SELECT {MODEL_COLUMNS} FROM model_configs WHERE model_hash = ?");
    sqlx::query(&query)
        .bind(model_hash)
        .fetch_optional(&mut **transaction)
        .await?
        .map(model_from_row)
        .transpose()
}

async fn update_model_in_transaction(
    transaction: &mut Transaction<'_, Sqlite>,
    model_hash: &str,
    config_hash: &str,
    input: &ModelConfigInput,
    now: i64,
) -> Result<()> {
    let result = sqlx::query(
        r#"UPDATE model_configs SET
            config_hash = ?, sort_order = ?, display_name = ?, group_name = ?, model_type = ?, base_url = ?,
            use_full_url = ?, api_key = ?, tooltip_data = ?, model_id = ?, supports_thinking = ?,
            supports_images = ?, supports_fast = ?, reasoning_effort = ?, openai_endpoint = ?,
            openai_extra_params_enabled = ?, openai_extra_params_json = ?, custom_headers_enabled = ?,
            custom_headers_json = ?, anthropic_extra_params_enabled = ?, anthropic_extra_params_json = ?,
            context_window_tokens = ?, max_completion_tokens = ?, anthropic_max_tokens = ?,
            anthropic_thinking_effort = ?, thinking_budget_tokens = ?, updated_at_ms = ?
        WHERE model_hash = ?"#,
    )
    .bind(config_hash)
    .bind(input.sort_order)
    .bind(&input.display_name)
    .bind(&input.group_name)
    .bind(input.model_type.as_str())
    .bind(&input.base_url)
    .bind(input.use_full_url)
    .bind(&input.api_key)
    .bind(&input.tooltip_data)
    .bind(&input.model_id)
    .bind(input.supports_thinking)
    .bind(input.supports_images)
    .bind(input.supports_fast)
    .bind(&input.reasoning_effort)
    .bind(&input.openai_endpoint)
    .bind(input.openai_extra_params_enabled)
    .bind(serde_json::to_string(&input.openai_extra_params)?)
    .bind(input.custom_headers_enabled)
    .bind(serde_json::to_string(&input.custom_headers)?)
    .bind(input.anthropic_extra_params_enabled)
    .bind(serde_json::to_string(&input.anthropic_extra_params)?)
    .bind(input.context_window_tokens.map(to_i64).transpose()?)
    .bind(input.max_completion_tokens.map(to_i64).transpose()?)
    .bind(input.anthropic_max_tokens.map(to_i64).transpose()?)
    .bind(&input.anthropic_thinking_effort)
    .bind(input.thinking_budget_tokens.map(to_i64).transpose()?)
    .bind(now)
    .bind(model_hash)
    .execute(&mut **transaction)
    .await?;
    if result.rows_affected() != 1 {
        return Err(Error::RunNotFound(format!("model {model_hash}")));
    }
    Ok(())
}

fn optional_u64(row: &sqlx::sqlite::SqliteRow, column: &str) -> Result<Option<u64>> {
    row.try_get::<Option<i64>, _>(column)?
        .map(|value| {
            u64::try_from(value).map_err(|_| Error::Config(format!("{column} cannot be negative")))
        })
        .transpose()
}

fn to_i64(value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|_| Error::Config("token value is too large".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn store() -> (tempfile::TempDir, Store) {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::connect(&format!(
            "sqlite://{}",
            directory.path().join("test.db").display()
        ))
        .await
        .unwrap();
        (directory, store)
    }

    fn model_input(group_name: Option<&str>) -> ModelConfigInput {
        ModelConfigInput {
            sort_order: 0,
            display_name: "Test Model".into(),
            group_name: group_name.map(String::from),
            model_type: ModelType::OpenAi,
            base_url: "https://example.com/v1/chat/completions".into(),
            use_full_url: true,
            api_key: "test-key".into(),
            tooltip_data: "Test Model".into(),
            model_id: "test-model".into(),
            supports_thinking: false,
            supports_images: false,
            supports_fast: false,
            reasoning_effort: None,
            openai_endpoint: crate::model::OPENAI_CHAT_ENDPOINT.into(),
            openai_extra_params_enabled: false,
            openai_extra_params: serde_json::json!({}),
            custom_headers_enabled: false,
            custom_headers: serde_json::json!({}),
            anthropic_extra_params_enabled: false,
            anthropic_extra_params: serde_json::json!({}),
            context_window_tokens: None,
            max_completion_tokens: None,
            anthropic_max_tokens: None,
            anthropic_thinking_effort: None,
            thinking_budget_tokens: None,
        }
    }

    #[tokio::test]
    async fn saved_anthropic_budget_is_validated_and_applied_to_invocations() {
        let (_directory, store) = store().await;
        let mut input = model_input(None);
        input.model_type = ModelType::Anthropic;
        input.supports_thinking = true;
        input.anthropic_max_tokens = Some(8192);
        for budget in [0, 1023, 8192, 9000] {
            input.thinking_budget_tokens = Some(budget);
            assert!(store.create_model(&input).await.is_err());
        }
        input.thinking_budget_tokens = Some(4096);
        let saved = store.create_model(&input).await.unwrap();
        let saved = store.model(&saved.model_hash).await.unwrap().unwrap();
        let mut request = crate::model::ModelSpec::new(&saved.model_hash);
        request.max_output_tokens = Some(30_000);
        request.context_window_tokens = Some(500_000);
        request.reasoning.effort = Some("low".into());
        saved.configure(&mut request);
        assert_eq!(request.max_output_tokens, Some(8192));
        assert_eq!(request.reasoning.budget_tokens, Some(4096));
        assert!(request.reasoning.enabled);
        assert_eq!(request.reasoning.effort.as_deref(), Some("low"));
        assert_eq!(request.context_window_tokens, Some(500_000));
        request.max_output_tokens = Some(2048);
        saved.configure(&mut request);
        assert_eq!(request.max_output_tokens, Some(2048));
    }

    /// 分组名是纯展示字段:入库时去除首尾空白、空串归一为 NULL,
    /// 更新分组名不得改变模型身份哈希。
    #[tokio::test]
    async fn group_name_round_trips_without_changing_model_identity() {
        let (_directory, store) = store().await;

        let created = store
            .create_model(&model_input(Some("  My Group  ")))
            .await
            .unwrap();
        assert_eq!(created.group_name.as_deref(), Some("My Group"));

        let renamed = store
            .update_model(&created.model_hash, &model_input(Some("Renamed")))
            .await
            .unwrap();
        assert_eq!(renamed.model_hash, created.model_hash);
        assert_eq!(renamed.group_name.as_deref(), Some("Renamed"));

        let cleared = store
            .update_model(&created.model_hash, &model_input(Some("   ")))
            .await
            .unwrap();
        assert_eq!(cleared.model_hash, created.model_hash);
        assert_eq!(cleared.group_name, None);
    }

    #[tokio::test]
    async fn model_identity_and_history_survive_configuration_edits_and_deletion() {
        let (_directory, store) = store().await;
        let created = store.create_model(&model_input(None)).await.unwrap();
        assert!(created.model_hash.starts_with(BUILTIN_MODEL_ID_PREFIX));

        sqlx::query(
            r#"INSERT INTO llm_calls(
                call_id, run_id, conversation_id, provider_call_index, model_hash,
                provider_type, provider_url, request_type, request_url, model_id,
                display_name, status, created_at_ms, message_count, tool_count, detailed
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
        )
        .bind("call-1")
        .bind("run-1")
        .bind("conversation-1")
        .bind(0_i64)
        .bind(&created.model_hash)
        .bind("openai-chat")
        .bind("https://example.com")
        .bind("openai-chat")
        .bind("https://example.com/v1/chat/completions")
        .bind("test-model")
        .bind("Test Model")
        .bind("completed")
        .bind(1_i64)
        .bind(1_i64)
        .bind(0_i64)
        .bind(false)
        .execute(store.pool())
        .await
        .unwrap();
        store
            .set_commit_settings(CommitSettings {
                model_id: created.model_hash.clone(),
                prompt: "keep prompt".into(),
                ..CommitSettings::default()
            })
            .await
            .unwrap();

        let mut changed = model_input(None);
        changed.display_name = "Renamed Model".into();
        changed.base_url = "https://other.example/v1/chat/completions".into();
        changed.api_key = "replacement-key".into();
        changed.model_id = "replacement-model".into();
        let updated = store
            .update_model(&created.model_hash, &changed)
            .await
            .unwrap();

        assert_eq!(updated.model_hash, created.model_hash);
        assert_ne!(updated.config_hash, created.config_hash);
        let historical_model: String =
            sqlx::query_scalar("SELECT model_hash FROM llm_calls WHERE call_id = 'call-1'")
                .fetch_one(store.pool())
                .await
                .unwrap();
        assert_eq!(historical_model, created.model_hash);
        assert_eq!(
            store.commit_settings().await.unwrap().model_id,
            created.model_hash
        );

        store.delete_model(&created.model_hash).await.unwrap();
        let historical_model: String =
            sqlx::query_scalar("SELECT model_hash FROM llm_calls WHERE call_id = 'call-1'")
                .fetch_one(store.pool())
                .await
                .unwrap();
        assert_eq!(historical_model, created.model_hash);
        let commit = store.commit_settings().await.unwrap();
        assert!(commit.is_direct());
        assert_eq!(commit.prompt, "keep prompt");
    }

    #[tokio::test]
    async fn model_update_preserves_api_key_when_the_input_is_empty() {
        let (_directory, store) = store().await;
        let created = store.create_model(&model_input(None)).await.unwrap();
        let mut changed = model_input(None);
        changed.api_key.clear();
        changed.display_name = "Renamed".into();

        let updated = store
            .update_model(&created.model_hash, &changed)
            .await
            .unwrap();

        assert_eq!(updated.api_key, "test-key");
        assert_eq!(updated.display_name, "Renamed");
    }

    #[tokio::test]
    async fn grouped_model_updates_roll_back_when_a_later_update_conflicts() {
        let (_directory, store) = store().await;
        let first_input = model_input(Some("Original"));
        let mut second_input = model_input(Some("Original"));
        second_input.display_name = "Second Model".into();
        second_input.model_id = "second-model".into();
        let created = store
            .create_models(&[first_input.clone(), second_input])
            .await
            .unwrap();

        let mut first_update = first_input.clone();
        first_update.group_name = Some("Changed".into());
        let result = store
            .update_models(&[
                ModelUpdate {
                    model_hash: created[0].model_hash.clone(),
                    model: first_update,
                },
                ModelUpdate {
                    model_hash: created[1].model_hash.clone(),
                    model: first_input,
                },
            ])
            .await;

        assert!(matches!(result, Err(Error::Database(_))));
        let first = store.model(&created[0].model_hash).await.unwrap().unwrap();
        let second = store.model(&created[1].model_hash).await.unwrap().unwrap();
        assert_eq!(first.group_name.as_deref(), Some("Original"));
        assert_eq!(second.display_name, "Second Model");
    }
}
