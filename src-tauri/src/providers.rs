use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;

use crate::config::ConfigState;
use crate::gateway;
use crate::model::{AssetRef, ProviderInfo, RunNodeRequest};

#[async_trait]
pub trait ModelProvider: Send + Sync {
    fn id(&self) -> &'static str;
    fn name(&self) -> &'static str;
    async fn generate_image(
        &self,
        req: &RunNodeRequest,
        out_dir: &Path,
    ) -> Result<Vec<AssetRef>, String>;
}

pub struct GatewayProvider {
    cfg: Arc<RwLock<ConfigState>>,
}

impl GatewayProvider {
    pub fn new(cfg: Arc<RwLock<ConfigState>>) -> Self {
        Self { cfg }
    }
}

#[async_trait]
impl ModelProvider for GatewayProvider {
    fn id(&self) -> &'static str {
        "zzone"
    }
    fn name(&self) -> &'static str {
        "ZZone 网关"
    }
    async fn generate_image(
        &self,
        req: &RunNodeRequest,
        out_dir: &Path,
    ) -> Result<Vec<AssetRef>, String> {
        // Clone under the lock so the guard is dropped before the await (keeps
        // the future Send).
        let cfg = self.cfg.read().unwrap().clone();
        gateway::generate_image(&cfg, req, out_dir).await
    }
}

pub struct ProviderRegistry {
    providers: RwLock<HashMap<&'static str, Arc<dyn ModelProvider>>>,
    active: RwLock<String>,
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self {
            providers: RwLock::new(HashMap::new()),
            active: RwLock::new("zzone".to_string()),
        }
    }

    pub fn register(&self, provider: Arc<dyn ModelProvider>) {
        let id = provider.id();
        self.providers.write().unwrap().insert(id, provider);
        *self.active.write().unwrap() = id.to_string();
    }

    pub fn active(&self) -> Arc<dyn ModelProvider> {
        let id = self.active.read().unwrap().clone();
        self.providers
            .read()
            .unwrap()
            .get(id.as_str())
            .cloned()
            .expect("active provider must be registered")
    }

    pub fn get(&self, id: &str) -> Option<Arc<dyn ModelProvider>> {
        self.providers.read().unwrap().get(id).cloned()
    }

    pub fn set_active(&self, id: String) -> Result<(), String> {
        if self.providers.read().unwrap().contains_key(id.as_str()) {
            *self.active.write().unwrap() = id;
            Ok(())
        } else {
            Err(format!("未知 provider: {id}"))
        }
    }

    pub fn list(&self) -> Vec<ProviderInfo> {
        let active = self.active.read().unwrap().clone();
        self.providers
            .read()
            .unwrap()
            .iter()
            .map(|(id, p)| ProviderInfo {
                id: id.to_string(),
                name: p.name().to_string(),
                active: active == *id,
                capabilities: vec![
                    "text_to_image".to_string(),
                    "image_to_image".to_string(),
                    "text_to_video".to_string(),
                    "image_to_video".to_string(),
                    "text_completion".to_string(),
                ],
            })
            .collect()
    }
}
