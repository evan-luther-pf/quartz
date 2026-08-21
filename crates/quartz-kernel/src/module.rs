use std::{
    cell::RefCell,
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Weak},
};

use sha2::{Digest, Sha256};
use wasmparser::{Parser, Payload};
use wasmtime::{Engine, component::Component};

use crate::{Error, Manifest, Result, manifest::MANIFEST_SECTION};

pub(crate) struct Artifact {
    pub path: PathBuf,
    pub digest: String,
    pub manifest: Manifest,
    pub component: Component,
}

pub(crate) struct ModuleLoader {
    engine: Engine,
    cache: RefCell<BTreeMap<(PathBuf, String), Weak<Artifact>>>,
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

    pub fn load(&self, path: &Path, expected_digest: Option<&str>) -> Result<Arc<Artifact>> {
        let path = std::fs::canonicalize(path).map_err(|source| Error::ReadArtifact {
            path: path.to_path_buf(),
            source,
        })?;
        let bytes = std::fs::read(&path).map_err(|source| Error::ReadArtifact {
            path: path.clone(),
            source,
        })?;
        let digest = sha256_hex(&bytes);
        if let Some(expected) = expected_digest
            && expected != digest
        {
            return Err(Error::ArtifactDigestMismatch {
                path,
                expected: expected.into(),
                actual: digest,
            });
        }
        let cache_key = (path.clone(), digest.clone());
        if let Some(artifact) = self.cache.borrow().get(&cache_key).and_then(Weak::upgrade) {
            return Ok(artifact);
        }

        let manifest = parse_manifest(&path, &bytes)?;
        manifest.validate()?;
        let component = Component::new(&self.engine, &bytes)
            .map_err(|error| Error::ParseComponent(error.to_string()))?;
        let artifact = Arc::new(Artifact {
            path,
            digest,
            manifest,
            component,
        });
        self.cache
            .borrow_mut()
            .insert(cache_key, Arc::downgrade(&artifact));
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

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        write!(output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}
