// ABOUTME: Persists only public synthetic pairing fixtures in caller-owned private test directories.
// ABOUTME: Qualifies atomic catalog replacement and readback without a production credential backend.
use earph_sendspin::pairing::{PairingRecord, RecordCatalog, RecordUpdate};
use serde_json::{Value, json};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub struct FixtureStore {
    directory: PathBuf,
}
impl FixtureStore {
    /// The directory must already exist. No home, config, or vault path is inferred.
    pub fn open(directory: &Path) -> Self {
        assert!(directory.is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                directory.metadata().unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        Self {
            directory: directory.to_owned(),
        }
    }
    pub fn allocate(name: &str) -> Self {
        let root = std::env::var_os("EARPH_PAIRING_FIXTURE_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("earph-synthetic-pairing-fixtures"));
        fs::create_dir_all(&root).unwrap();
        let directory = root.join(format!(
            "{name}-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&directory).unwrap();
        Self::open(&directory)
    }
    pub fn directory(&self) -> &Path {
        &self.directory
    }
    pub fn commit(&self, update: &RecordUpdate) {
        self.commit_with_boundary(update, false);
    }
    /// A real rename followed by a lost local result exercises uncertain recovery.
    pub fn commit_with_boundary(&self, update: &RecordUpdate, lose_after_replace: bool) {
        let stage = self.stage(update);
        fs::rename(&stage, self.directory.join("committed.json")).unwrap();
        if lose_after_replace {
            return;
        }
        File::open(&self.directory).unwrap().sync_all().unwrap();
        assert_eq!(self.read(), update.replacement);
    }
    pub fn stage(&self, update: &RecordUpdate) -> PathBuf {
        let snapshot = &update.replacement;
        let value = json!({"capacity":snapshot.capacity(),"superseded":snapshot.superseded_ids(),
            "records":snapshot.records().iter().map(|r|json!({"server":r.server_public.to_vec(),"psk":r.psk.to_vec(),"order":r.last_used_order})).collect::<Vec<_>>()});
        let stage = self.directory.join(format!(
            "staged-{}",
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&stage).unwrap();
        file.write_all(&serde_json::to_vec(&value).unwrap())
            .unwrap();
        file.sync_all().unwrap();
        drop(file);
        stage
    }
    pub fn read(&self) -> RecordCatalog {
        let path = self.directory.join("committed.json");
        if !path.exists() {
            return RecordCatalog::new(5).unwrap();
        }
        let value: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        let records = value["records"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| PairingRecord {
                server_public: key(&r["server"]),
                psk: key(&r["psk"]),
                last_used_order: r["order"].as_u64().unwrap(),
            })
            .collect();
        let superseded = value["superseded"]
            .as_array()
            .unwrap()
            .iter()
            .map(key)
            .collect();
        RecordCatalog::from_committed(
            value["capacity"].as_u64().unwrap() as usize,
            records,
            superseded,
        )
        .unwrap()
    }
}
fn key(value: &Value) -> [u8; 32] {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|v| u8::try_from(v.as_u64().unwrap()).unwrap())
        .collect::<Vec<_>>()
        .try_into()
        .unwrap()
}
