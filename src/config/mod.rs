use std::env;

pub struct Config {
    pub host: String,
    pub port: u16,
    pub ha_url: String,
    pub ha_token: String,
}

impl Config {
    pub fn from_env() -> Result<Self, Box<dyn std::error::Error>> {
        let host = env::var("HEMS_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());

        let port = env::var("HEMS_PORT")
            .ok()
            .and_then(|value| value.parse::<u16>().ok())
            .unwrap_or(3001);

        let ha_url = env::var("HA_URL").unwrap_or_else(|_| "http://192.168.1.4:8123".to_string());

        let ha_token =
            env::var("HA_TOKEN").map_err(|_| "HA_TOKEN environment variable is required")?;

        if ha_token.trim().is_empty() {
            return Err("HA_TOKEN must not be empty".into());
        }

        Ok(Self {
            host,
            port,
            ha_url,
            ha_token,
        })
    }
}
