use std::{
    cell::RefCell,
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Weak},
};

use wasmparser::{Parser, Payload};
use wasmtime::{Engine, component::Component};

use crate::{Error, Manifest, Result, manifest::MANIFEST_SECTION};

pub(crate) struct Artifact {
    pub path: PathBuf,
    pub manifest: Manifest,
    pub component: Component,
}

pub(crate) struct ModuleLoader {
    engine: Engine,
    cache: RefCell<BTreeMap<PathBuf, Weak<Artifact>>>,
}

impl ModuleLoader {
    pub fn new() -> Result<Self> {
        let engine = Engine::default();
        Ok(Self {
            engine,
            cache: RefCell::new(BTreeMap::new()),
        })
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    pub fn load(&self, path: &Path) -> Result<Arc<Artifact>> {
        let path = std::fs::canonicalize(path).map_err(|source| Error::ReadArtifact {
            path: path.to_path_buf(),
            source,
        })?;
        if let Some(artifact) = self.cache.borrow().get(&path).and_then(Weak::upgrade) {
            return Ok(artifact);
        }

        let bytes = std::fs::read(&path).map_err(|source| Error::ReadArtifact {
            path: path.clone(),
            source,
        })?;
        let manifest = parse_manifest(&path, &bytes)?;
        manifest.validate()?;
        let component = Component::new(&self.engine, &bytes)
            .map_err(|error| Error::ParseComponent(error.to_string()))?;
        let artifact = Arc::new(Artifact {
            path: path.clone(),
            manifest,
            component,
        });
        self.cache
            .borrow_mut()
            .insert(path, Arc::downgrade(&artifact));
        Ok(artifact)
    }

    pub fn live_artifact_count(&self) -> usize {
        let mut cache = self.cache.borrow_mut();
        cache.retain(|_, artifact| artifact.strong_count() > 0);
        cache
            .values()
            .filter(|artifact| artifact.strong_count() > 0)
            .count()
    }
}

fn parse_manifest(path: &Path, bytes: &[u8]) -> Result<Manifest> {
    for payload in Parser::new(0).parse_all(bytes) {
        match payload.map_err(|error| Error::ParseComponent(error.to_string()))? {
            Payload::CustomSection(section) if section.name() == MANIFEST_SECTION => {
                return serde_json::from_slice(section.data()).map_err(Error::from);
            }
            _ => {}
        }
    }
    Err(Error::MissingManifest(path.to_path_buf()))
}
