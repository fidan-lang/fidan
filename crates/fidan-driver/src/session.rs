// fidan-driver stubs — Phase 1+
use fidan_source::SourceMap;
use std::sync::Arc;

pub struct Session {
    pub source_map: Arc<SourceMap>,
}

impl Session {
    pub fn new() -> Self {
        Session {
            source_map: Arc::new(SourceMap::new()),
        }
    }
}
