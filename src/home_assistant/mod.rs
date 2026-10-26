use reqwest::Client;
use serde::Deserialize;
use std::time::Duration;

#[derive(Debug, Deserialize)]
struct EntityState {
    state: String,
}

#[derive(Clone)]
pub struct HomeAssistantClient {
    client: Client,
    base_url: String,
    token: String,
}

impl HomeAssistantClient {
    pub fn new(base_url: String, token: String) -> Result<Self, reqwest::Error> {
        let client = Client::builder().timeout(Duration::from_secs(10)).build()?;

        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            token,
        })
    }

    pub async fn get_numeric_state(
        &self,
        entity_id: &str,
    ) -> Result<f64, Box<dyn std::error::Error + Send + Sync>> {
        let url = format!("{}/api/states/{}", self.base_url, entity_id);

        let response = self
            .client
            .get(&url)
            .bearer_auth(&self.token)
            .send()
            .await?
            .error_for_status()?;

        let entity: EntityState = response.json().await?;

        let value: f64 = entity.state.parse()?;

        if !value.is_finite() {
            return Err(format!("Non-finite value for {entity_id}").into());
        }

        Ok(value)
    }
}
