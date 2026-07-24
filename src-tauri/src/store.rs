use crate::model::{
    default_copy_shortcut, default_mouse_shortcut, default_paste_shortcut, ScreenPosition, Settings,
};
use std::{fs, io, path::PathBuf, sync::RwLock};
use uuid::Uuid;

pub struct Store {
    path: PathBuf,
    value: RwLock<Settings>,
}

impl Store {
    pub fn load(app_dir: PathBuf) -> io::Result<Self> {
        fs::create_dir_all(&app_dir)?;
        let path = app_dir.join("settings.json");
        let value = fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_else(|| Settings {
                device_id: Uuid::new_v4().to_string(),
                device_name: hostname::get()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
                peers: Vec::new(),
                sync_enabled: true,
                launch_at_login: false,
                copy_shortcut: default_copy_shortcut(),
                paste_shortcut: default_paste_shortcut(),
                mouse_share_enabled: false,
                mouse_shortcut: default_mouse_shortcut(),
                mouse_position: ScreenPosition::Right,
            });
        let store = Self {
            path,
            value: RwLock::new(value),
        };
        store.save()?;
        Ok(store)
    }

    pub fn get(&self) -> Settings {
        self.value.read().expect("settings lock poisoned").clone()
    }

    pub fn update(&self, mutate: impl FnOnce(&mut Settings)) -> io::Result<Settings> {
        let snapshot = {
            let mut value = self.value.write().expect("settings lock poisoned");
            mutate(&mut value);
            value.clone()
        };
        self.save()?;
        Ok(snapshot)
    }

    fn save(&self) -> io::Result<()> {
        let data = serde_json::to_vec_pretty(&*self.value.read().expect("settings lock poisoned"))?;
        let temp = self.path.with_extension("tmp");
        fs::write(&temp, data)?;
        fs::rename(temp, &self.path)
    }
}
