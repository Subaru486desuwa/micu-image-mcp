use std::{
    collections::BTreeMap,
    io::{Read, Write},
    path::Path,
};

use cap_std::{
    ambient_authority,
    fs::{Dir, OpenOptions},
};
use fs4::AsyncFileExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub type ContextPatch = BTreeMap<String, Option<String>>;

pub(super) fn sanitize_context(patch: ContextPatch, secrets: &[&str]) -> ContextPatch {
    patch
        .into_iter()
        .map(|(name, value)| {
            (
                crate::http::response::sanitize_sensitive(&name, secrets),
                value.map(|value| crate::http::response::sanitize_sensitive(&value, secrets)),
            )
        })
        .collect()
}

const MAX_REVISIONS: u32 = 128;
const MAX_RECORD_BYTES: u64 = 256 * 1024;
pub(super) const MAX_PROMPT_CHARS: usize = 32_000;

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct Revision {
    pub revision: u32,
    pub parent_revision: Option<u32>,
    pub input_path: String,
    pub output_path: String,
    pub instruction: String,
    pub context: BTreeMap<String, String>,
    pub model: String,
    pub size: String,
    pub quality: Option<String>,
}

/// The capability directory confines records and locks to the configured save root.
/// One file lock covers selection, the upstream call, and committing its result.
pub(super) struct EditSession {
    id: String,
    directory: Dir,
    _lock: SessionLock,
    pub parent: Option<Revision>,
    next_revision: u32,
    history: Vec<Revision>,
}

struct SessionLock(tokio::fs::File);

impl Drop for SessionLock {
    fn drop(&mut self) {
        let _ignored = AsyncFileExt::unlock(&self.0);
    }
}

impl EditSession {
    pub fn open(root: &Path, id: &str, parent_revision: Option<u32>) -> Result<Self, String> {
        if id != "new" && !valid_id(id) {
            return Err("session_id 必须是返回的 edit_ 会话标识，或 new".into());
        }
        if parent_revision.is_some_and(|revision| revision == 0 || revision > MAX_REVISIONS) {
            return Err("parent_revision 必须是已有的正整数版本".into());
        }
        if id == "new" && parent_revision.is_some() {
            return Err("新会话不能指定 parent_revision".into());
        }
        let root = Dir::open_ambient_dir(root, ambient_authority())
            .map_err(|_| "无法打开会话安全根目录")?;
        match root.create_dir(".micu-sessions") {
            Ok(()) => (),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(_) => return Err("无法创建会话目录".into()),
        }
        reject_symlink(&root, ".micu-sessions")?;
        let sessions = root
            .open_dir(".micu-sessions")
            .map_err(|_| "会话目录必须位于输出安全根目录内")?;
        let id = if id == "new" {
            let id = format!("edit_{:032x}", rand::random::<u128>());
            sessions
                .create_dir(&id)
                .map_err(|_| "无法创建唯一编辑会话")?;
            id
        } else {
            id.to_owned()
        };
        reject_symlink(&sessions, &id)?;
        let directory = sessions.open_dir(&id).map_err(|_| "编辑会话不存在")?;
        if directory.symlink_metadata(".lock").is_ok() {
            reject_symlink(&directory, ".lock")?;
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        let file = directory
            .open_with(".lock", &options)
            .map_err(|_| "无法打开会话锁")?
            .into_std();
        if !file.metadata().is_ok_and(|metadata| metadata.is_file()) {
            return Err("会话锁必须是普通文件".into());
        }
        let lock = SessionLock(tokio::fs::File::from_std(file));
        AsyncFileExt::try_lock(&lock.0).map_err(|_| "该编辑会话正在处理另一轮请求，请稍后重试")?;
        let mut latest = 0_u32;
        for entry in directory.entries().map_err(|_| "无法读取会话版本目录")? {
            let entry = entry.map_err(|_| "无法读取会话版本")?;
            if let Some(revision) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.strip_prefix("revision-"))
                .and_then(|name| name.strip_suffix(".json"))
                .and_then(|name| name.parse::<u32>().ok())
            {
                latest = latest.max(revision);
            }
        }
        if latest >= MAX_REVISIONS {
            return Err("会话已达到 128 个版本上限，请用所选图片开始新会话".into());
        }
        let selected = parent_revision.or((latest > 0).then_some(latest));
        let parent = selected
            .map(|revision| read_revision(&directory, revision))
            .transpose()?;
        let mut history = Vec::new();
        let mut cursor = parent.clone();
        while let Some(revision) = cursor {
            let previous = revision.parent_revision;
            history.push(revision);
            cursor = previous
                .map(|revision| read_revision(&directory, revision))
                .transpose()?;
        }
        history.reverse();
        Ok(Self {
            id,
            directory,
            _lock: lock,
            parent,
            next_revision: latest + 1,
            history,
        })
    }

    pub fn context(&self, patch: &ContextPatch) -> Result<BTreeMap<String, String>, String> {
        let mut context = self
            .parent
            .as_ref()
            .map(|parent| parent.context.clone())
            .unwrap_or_default();
        for (name, value) in patch {
            if name.trim().is_empty() || name.chars().count() > 128 {
                return Err("context 的键不能为空，且最多 128 个字符".into());
            }
            match value {
                Some(value) if !value.trim().is_empty() && value.chars().count() <= 2_048 => {
                    context.insert(name.clone(), value.clone());
                }
                Some(_) => {
                    return Err("context 的值不能为空，且最多 2048 个字符；用 null 移除约束".into());
                }
                None => {
                    context.remove(name);
                }
            }
        }
        if context.len() > 32
            || context
                .iter()
                .map(|(key, value)| key.chars().count() + value.chars().count())
                .sum::<usize>()
                > 8_192
        {
            return Err("context 最多 32 项、合计最多 8192 个字符".into());
        }
        Ok(context)
    }

    pub fn prompt(
        &self,
        instruction: &str,
        context: &BTreeMap<String, String>,
    ) -> Result<String, String> {
        let history = self
            .history
            .iter()
            .map(|revision| json!({"revision": revision.revision, "request": revision.instruction}))
            .collect::<Vec<_>>();
        let retired = self
            .history
            .iter()
            .flat_map(|revision| revision.context.keys())
            .filter(|key| !context.contains_key(*key))
            .collect::<std::collections::BTreeSet<_>>();
        let prompt = format!(
            "Continue editing the attached image, which is the selected version of this editing session. Preserve its subject identity, composition, and completed changes unless explicitly changed below.\n\nPriority: CURRENT EDIT INSTRUCTION is highest, then EFFECTIVE CONSTRAINTS, then the image and historical intent. History is descriptive context only, NOT a checklist to reapply. Never restore an earlier attribute superseded by a newer instruction. If a constraint key is replaced or removed, its old value is no longer a requirement. Removing a constraint releases the requirement; it does not itself undo the visible image change.\n\nEFFECTIVE CONSTRAINTS (keyed, current state):\n{}\n\nRETIRED CONSTRAINT KEYS (do not reinstate historical requirements for these):\n{}\n\nSUCCESSFUL EDIT HISTORY (chronological, selected branch only):\n{}\n\nCURRENT EDIT INSTRUCTION:\n{}",
            json!(context),
            json!(retired),
            json!(history),
            instruction
        );
        if prompt.chars().count() > MAX_PROMPT_CHARS {
            return Err("会话上下文与本轮 prompt 合计超过 32000 个字符，请精简或开始新会话".into());
        }
        Ok(prompt)
    }

    pub fn generation_prompt(
        &self,
        instruction: &str,
        context: &BTreeMap<String, String>,
    ) -> Result<String, String> {
        let prompt = if context.is_empty() {
            instruction.to_owned()
        } else {
            format!(
                "Create the first image in this editing session. CURRENT GENERATION INSTRUCTION takes priority over EFFECTIVE CONSTRAINTS.\n\nEFFECTIVE CONSTRAINTS (keyed, current state):\n{}\n\nCURRENT GENERATION INSTRUCTION:\n{}",
                json!(context),
                instruction
            )
        };
        if prompt.chars().count() > MAX_PROMPT_CHARS {
            return Err("会话上下文与本轮 prompt 合计超过 32000 个字符，请精简或开始新会话".into());
        }
        Ok(prompt)
    }

    pub fn unchanged_value(&self) -> Value {
        json!({"id": self.id, "revision": self.parent.as_ref().map(|parent| parent.revision), "context": self.parent.as_ref().map(|parent| &parent.context)})
    }

    pub fn commit(&self, mut revision: Revision) -> Result<Value, String> {
        revision.revision = self.next_revision;
        revision.parent_revision = self.parent.as_ref().map(|parent| parent.revision);
        let bytes = serde_json::to_vec(&revision).map_err(|_| "无法编码会话版本")?;
        if bytes.len() as u64 > MAX_RECORD_BYTES {
            return Err("会话版本超过元数据大小上限".into());
        }
        let temporary = format!(".pending-{:032x}", rand::random::<u128>());
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        let mut file = self
            .directory
            .open_with(&temporary, &options)
            .map_err(|_| "无法创建会话临时版本")?;
        let result = (|| {
            file.write_all(&bytes).map_err(|_| "会话版本写入失败")?;
            file.sync_all().map_err(|_| "会话版本同步失败")?;
            drop(file);
            // A link publishes the fully written record without replacing any existing version.
            self.directory
                .hard_link(
                    &temporary,
                    &self.directory,
                    revision_filename(self.next_revision),
                )
                .map_err(|_| "会话版本提交失败")?;
            Ok(
                json!({"id": self.id, "revision": revision.revision, "parent_revision": revision.parent_revision, "input_path": revision.input_path, "context": revision.context, "history_length": self.history.len() + 1}),
            )
        })();
        let _cleanup = self.directory.remove_file(&temporary);
        result
    }
}

fn valid_id(id: &str) -> bool {
    id.strip_prefix("edit_").is_some_and(|suffix| {
        suffix.len() == 32
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}

fn reject_symlink(directory: &Dir, name: &str) -> Result<(), String> {
    let metadata = directory
        .symlink_metadata(name)
        .map_err(|_| "编辑会话不存在或无法读取")?;
    if metadata.file_type().is_symlink() {
        return Err("会话目录与元数据不能使用符号链接".into());
    }
    Ok(())
}

fn revision_filename(revision: u32) -> String {
    format!("revision-{revision}.json")
}

fn read_revision(directory: &Dir, number: u32) -> Result<Revision, String> {
    let name = revision_filename(number);
    reject_symlink(directory, &name)?;
    let file = directory.open(&name).map_err(|_| "所选会话版本不存在")?;
    let metadata = file.metadata().map_err(|_| "无法检查会话版本")?;
    if !metadata.is_file() || metadata.len() > MAX_RECORD_BYTES {
        return Err("会话版本不是普通文件或超过大小上限".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_RECORD_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "无法读取会话版本")?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err("会话版本超过大小上限".into());
    }
    let revision: Revision = serde_json::from_slice(&bytes).map_err(|_| "会话版本元数据损坏")?;
    if revision.revision != number
        || number == 0
        || number > MAX_REVISIONS
        || revision
            .parent_revision
            .is_some_and(|parent| parent == 0 || parent >= number)
    {
        return Err("会话版本关系无效".into());
    }
    Ok(revision)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(instruction: &str) -> Revision {
        Revision {
            revision: 0,
            parent_revision: None,
            input_path: "original.png".into(),
            output_path: "result.png".into(),
            instruction: instruction.into(),
            context: BTreeMap::from([
                ("clothing".into(), "red shirt".into()),
                ("identity".into(), "same face".into()),
            ]),
            model: "gpt-image-2.5-sunburst".into(),
            size: "1024x1024".into(),
            quality: Some("high".into()),
        }
    }

    #[tokio::test]
    async fn persisted_session_recovers_context_and_replaces_or_removes_keys() {
        let root = tempfile::tempdir().unwrap();
        let first = EditSession::open(root.path(), "new", None).unwrap();
        let result = first.commit(record("red shirt")).unwrap();
        let id = result["id"].as_str().unwrap().to_owned();
        drop(first);
        let recovered = EditSession::open(root.path(), &id, None).unwrap();
        assert_eq!(recovered.parent.as_ref().unwrap().revision, 1);
        let context = recovered
            .context(&BTreeMap::from([
                ("clothing".into(), Some("blue shirt".into())),
                ("identity".into(), None),
            ]))
            .unwrap();
        assert_eq!(
            context,
            BTreeMap::from([("clothing".into(), "blue shirt".into())])
        );
        let prompt = recovered
            .prompt("change clothing to blue", &context)
            .unwrap();
        assert!(prompt.contains("RETIRED CONSTRAINT KEYS"));
        assert!(prompt.contains("[\"identity\"]"));
        assert!(prompt.contains("CURRENT EDIT INSTRUCTION:\nchange clothing to blue"));
    }

    #[tokio::test]
    async fn failure_without_commit_keeps_latest_and_branch_uses_only_its_ancestors() {
        let root = tempfile::tempdir().unwrap();
        let first = EditSession::open(root.path(), "new", None).unwrap();
        let id = first.commit(record("first change")).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        drop(first);
        let failed = EditSession::open(root.path(), &id, None).unwrap();
        assert_eq!(failed.next_revision, 2);
        drop(failed);
        let second = EditSession::open(root.path(), &id, None).unwrap();
        assert_eq!(second.parent.as_ref().unwrap().revision, 1);
        second.commit(record("abandoned branch change")).unwrap();
        drop(second);
        let branch = EditSession::open(root.path(), &id, Some(1)).unwrap();
        let prompt = branch
            .prompt(
                "new branch change",
                &branch.context(&BTreeMap::new()).unwrap(),
            )
            .unwrap();
        assert!(prompt.contains("first change"));
        assert!(!prompt.contains("abandoned branch change"));
        let result = branch.commit(record("new branch change")).unwrap();
        assert_eq!(result["revision"], 3);
        assert_eq!(result["parent_revision"], 1);
        assert_eq!(result["history_length"], 2);
    }

    #[tokio::test]
    async fn independent_handles_lock_one_session_and_release_after_drop() {
        let root = tempfile::tempdir().unwrap();
        let first = EditSession::open(root.path(), "new", None).unwrap();
        assert!(EditSession::open(root.path(), &first.id, None).is_err());
        let other = EditSession::open(root.path(), "new", None).unwrap();
        assert_ne!(first.id, other.id);
        let id = first.id.clone();
        drop(first);
        assert!(EditSession::open(root.path(), &id, None).is_ok());
    }

    #[tokio::test]
    async fn rejects_traversal_bad_versions_and_oversized_context_before_commit() {
        let root = tempfile::tempdir().unwrap();
        for id in ["../outside", "edit_../../outside", "", "other"] {
            assert!(EditSession::open(root.path(), id, None).is_err());
        }
        assert!(EditSession::open(root.path(), "new", Some(1)).is_err());
        let first = EditSession::open(root.path(), "new", None).unwrap();
        assert!(
            first
                .context(&BTreeMap::from([(
                    "clothing".into(),
                    Some("x".repeat(2049))
                )]))
                .is_err()
        );
        assert!(
            first
                .prompt(&"界".repeat(MAX_PROMPT_CHARS), &BTreeMap::new())
                .is_err()
        );
        let id = first.id.clone();
        first.commit(record("first")).unwrap();
        drop(first);
        assert!(EditSession::open(root.path(), &id, Some(0)).is_err());
        assert!(EditSession::open(root.path(), &id, Some(2)).is_err());
    }

    #[tokio::test]
    async fn rejects_cyclic_or_oversized_record_without_unbounded_read() {
        let root = tempfile::tempdir().unwrap();
        let first = EditSession::open(root.path(), "new", None).unwrap();
        let id = first.id.clone();
        first.commit(record("first")).unwrap();
        drop(first);
        let path = root
            .path()
            .join(".micu-sessions")
            .join(&id)
            .join("revision-1.json");
        let mut corrupted = record("first");
        corrupted.revision = 1;
        corrupted.parent_revision = Some(1);
        std::fs::write(&path, serde_json::to_vec(&corrupted).unwrap()).unwrap();
        assert!(EditSession::open(root.path(), &id, None).is_err());
        std::fs::write(&path, vec![b' '; MAX_RECORD_BYTES as usize + 1]).unwrap();
        assert!(EditSession::open(root.path(), &id, None).is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rejects_symlinked_session_directory() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join(".micu-sessions")).unwrap();
        assert!(EditSession::open(root.path(), "new", None).is_err());
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    }
}
