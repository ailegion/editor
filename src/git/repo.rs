//! Read-only, in-process access to a repository's committed and staged file versions through
//! gitoxide. Nothing here writes to the repository; changes go through [`super::cli`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use gix::bstr::ByteSlice;
use gix::filter::plumbing::pipeline::convert::ToGitOutcome;

type Blob = Arc<[u8]>;

/// A file as `git` would store it, converted from the working tree.
pub enum Worktree {
    Missing,
    Content(Vec<u8>),
    /// The file uses an external filter driver (e.g. Git LFS), which only `git` itself runs.
    NeedsDriver,
}

pub struct Repo {
    repo: gix::ThreadSafeRepository,
    workdir: PathBuf,
    /// The project root's location inside the working tree, `""` when they are the same.
    prefix: PathBuf,
    /// Blob contents by object id; ids are content hashes, so entries never go stale.
    blobs: Mutex<HashMap<gix::ObjectId, Blob>>,
}

const BLOB_CACHE_ENTRIES: usize = 128;

impl Repo {
    /// Finds the repository containing `project_root`, or `None` when it is not inside one.
    pub fn discover(project_root: &Path) -> Option<Repo> {
        let repo = gix::discover(project_root).ok()?;
        let workdir = repo.workdir()?.to_path_buf();
        let canonical_root = dunce_canonicalize(project_root)?;
        let prefix = canonical_root.strip_prefix(dunce_canonicalize(&workdir)?).ok()?.to_path_buf();
        Some(Repo { repo: repo.into_sync(), workdir, prefix, blobs: Mutex::default() })
    }

    pub fn workdir(&self) -> &Path { &self.workdir }

    pub fn thread_local(&self) -> gix::Repository { self.repo.to_thread_local() }

    pub fn git_dir(&self) -> PathBuf { self.repo.git_dir().to_path_buf() }

    pub fn common_dir(&self) -> PathBuf { self.thread_local().common_dir().to_path_buf() }

    /// Converts a path relative to the project root into git's slash-separated, repository-relative form.
    fn rela(&self, path: &Path) -> String {
        let joined = self.prefix.join(path);
        joined.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/")
    }

    fn blob(&self, repo: &gix::Repository, id: gix::ObjectId) -> Result<Blob, String> {
        if let Some(blob) = self.blobs.lock().unwrap().get(&id) {
            return Ok(blob.clone());
        }
        let blob: Blob = repo.find_blob(id).map_err(|err| err.to_string())?.detach().data.into();
        let mut cache = self.blobs.lock().unwrap();
        if cache.len() >= BLOB_CACHE_ENTRIES { cache.clear(); }
        cache.insert(id, blob.clone());
        Ok(blob)
    }

    /// The staged version of `path` (relative to the project root). For a conflicted file this
    /// is "ours", like `git diff`'s default comparison.
    pub fn index_version(&self, path: &Path) -> Result<Option<Blob>, String> {
        let repo = self.repo.to_thread_local();
        let index = repo.index_or_empty().map_err(|err| err.to_string())?;
        let Some(entry) = index.entry_by_path(self.rela(path).as_bytes().as_bstr()) else { return Ok(None) };
        if entry.mode.is_submodule() { return Ok(None); }
        let id = entry.id;
        drop(index);
        self.blob(&repo, id).map(Some)
    }

    /// Whether the index holds unresolved merge stages for `path`.
    pub fn is_conflicted(&self, path: &Path) -> Result<bool, String> {
        let repo = self.repo.to_thread_local();
        let index = repo.index_or_empty().map_err(|err| err.to_string())?;
        let rela = self.rela(path);
        Ok(index.entries().iter().any(|entry| entry.stage_raw() != 0 && entry.path(&index) == rela.as_bytes()))
    }

    /// The version of `path` in the `HEAD` commit, `None` before the first commit or if absent there.
    pub fn head_version(&self, path: &Path) -> Result<Option<Blob>, String> {
        let repo = self.repo.to_thread_local();
        let tree_id = repo.head_tree_id_or_empty().map_err(|err| err.to_string())?.detach();
        let tree = repo.find_tree(tree_id).map_err(|err| err.to_string())?;
        let Some(entry) = tree.lookup_entry_by_path(self.prefix.join(path)).map_err(|err| err.to_string())? else {
            return Ok(None);
        };
        if !entry.mode().is_blob_or_symlink() { return Ok(None); }
        self.blob(&repo, entry.object_id()).map(Some)
    }

    /// The working-tree file converted the way `git add` would (line endings, `ident`,
    /// `working-tree-encoding`), without running external filter drivers.
    pub fn worktree_version(&self, path: &Path) -> Result<Worktree, String> {
        let repo = self.repo.to_thread_local();
        let rela = self.rela(path);
        let file = match std::fs::symlink_metadata(self.workdir.join(&rela)) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Worktree::Missing),
            Err(err) => return Err(err.to_string()),
            Ok(meta) if meta.is_symlink() => {
                let target = std::fs::read_link(self.workdir.join(&rela)).map_err(|err| err.to_string())?;
                return Ok(Worktree::Content(gix::path::into_bstr(target).to_vec()));
            }
            Ok(meta) if !meta.is_file() => return Ok(Worktree::Missing),
            Ok(_) => std::fs::read(self.workdir.join(&rela)).map_err(|err| err.to_string())?,
        };

        let (pipeline, index) = repo.filter_pipeline(None).map_err(|err| err.to_string())?;
        let (_, mut attributes) = pipeline.into_parts();
        let mut options = gix::filter::Pipeline::options(&repo).map_err(|err| err.to_string())?;
        let drivers = std::mem::take(&mut options.drivers);
        let rela_path = gix::path::from_bstr(rela.as_bytes().as_bstr()).into_owned();
        if uses_driver(&mut attributes, &rela_path, &drivers, &repo)? {
            return Ok(Worktree::NeedsDriver);
        }
        // The diff is display-only; report differences instead of refusing irreversible CRLF conversions.
        options.crlf_roundtrip_check = gix::filter::plumbing::pipeline::CrlfRoundTripCheck::Skip;
        let context = repo.command_context().map_err(|err| err.to_string())?;
        let mut filters = gix::filter::plumbing::Pipeline::new(context, options);
        let platform = attributes.at_path(rela_path.as_path(), None, &repo.objects).map_err(|err| err.to_string())?;
        let outcome = filters.convert_to_git(
            file.as_slice(),
            &rela_path,
            &mut |_, attrs| { platform.matching_attributes(attrs); },
            &mut |buf| {
                let Some(entry) = index.entry_by_path(rela.as_bytes().as_bstr()) else { return Ok(None) };
                use gix::prelude::Find;
                Ok(repo.objects.try_find(&entry.id, buf)?.filter(|obj| obj.kind == gix::object::Kind::Blob).map(|_| ()))
            },
        ).map_err(|err| err.to_string())?;
        let converted = match outcome {
            ToGitOutcome::Unchanged(_) => None,
            ToGitOutcome::Buffer(buf) => Some(buf.to_vec()),
            ToGitOutcome::Process(_) => return Ok(Worktree::NeedsDriver),
        };
        Ok(Worktree::Content(converted.unwrap_or(file)))
    }

    /// The staged text an open editor buffer is compared against, or `None` when `path` is
    /// untracked or stored through an external filter (its staged form is not the file's text).
    pub fn buffer_base(&self, path: &Path) -> Result<Option<Blob>, String> {
        let Some(blob) = self.index_version(path)? else { return Ok(None) };
        let repo = self.repo.to_thread_local();
        let index = repo.index_or_empty().map_err(|err| err.to_string())?;
        let mut attributes = repo
            .attributes_only(&index, gix::worktree::stack::state::attributes::Source::WorktreeThenIdMapping)
            .map_err(|err| err.to_string())?
            .detach();
        let drivers = gix::filter::Pipeline::options(&repo).map_err(|err| err.to_string())?.drivers;
        let rela_path = gix::path::from_bstr(self.rela(path).as_bytes().as_bstr()).into_owned();
        Ok((!uses_driver(&mut attributes, &rela_path, &drivers, &repo)?).then_some(blob))
    }
}

/// Whether `.gitattributes` assign `rela_path` a `filter` that has a configured driver.
fn uses_driver(
    attributes: &mut gix::worktree::Stack,
    rela_path: &Path,
    drivers: &[gix::filter::plumbing::Driver],
    repo: &gix::Repository,
) -> Result<bool, String> {
    if drivers.is_empty() { return Ok(false); }
    let mut outcome = attributes.selected_attribute_matches(["filter"]);
    let platform = attributes.at_path(rela_path, None, &repo.objects).map_err(|err| err.to_string())?;
    platform.matching_attributes(&mut outcome);
    Ok(outcome.iter_selected().any(|m| {
        matches!(m.assignment.state, gix::attrs::StateRef::Value(name) if drivers.iter().any(|d| d.name == name.as_bstr()))
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_with_external_filter_drivers_are_left_to_git() {
        let root = std::env::temp_dir().join(format!("editor-driver-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let git = |args: &[&str]| super::super::cli::run(&root, args, None, super::super::cli::Access::Write).unwrap();
        git(&["init"]);
        git(&["config", "filter.fake.clean", "cat"]);
        git(&["config", "filter.fake.smudge", "cat"]);
        std::fs::write(root.join(".gitattributes"), "*.bin filter=fake\n").unwrap();
        std::fs::write(root.join("data.bin"), "pointer\n").unwrap();
        std::fs::write(root.join("plain.txt"), "text\n").unwrap();
        git(&["add", "."]);
        let repo = Repo::discover(&root).unwrap();
        assert!(matches!(repo.worktree_version(Path::new("data.bin")).unwrap(), Worktree::NeedsDriver));
        assert!(repo.buffer_base(Path::new("data.bin")).unwrap().is_none());
        assert!(matches!(repo.worktree_version(Path::new("plain.txt")).unwrap(), Worktree::Content(c) if c == b"text\n"));
        assert_eq!(repo.buffer_base(Path::new("plain.txt")).unwrap().as_deref(), Some(&b"text\n"[..]));
        assert!(repo.head_version(Path::new("plain.txt")).unwrap().is_none(), "no commit yet");
        std::fs::remove_dir_all(root).unwrap();
    }
}

/// `std::fs::canonicalize` without Windows' `\\?\` prefix, so prefix stripping compares like paths.
pub(super) fn dunce_canonicalize(path: &Path) -> Option<PathBuf> {
    let canonical = std::fs::canonicalize(path).ok()?;
    #[cfg(windows)]
    {
        let text = canonical.to_string_lossy();
        if let Some(rest) = text.strip_prefix(r"\\?\UNC\") { return Some(PathBuf::from(format!(r"\\{rest}"))); }
        if let Some(rest) = text.strip_prefix(r"\\?\") { return Some(PathBuf::from(rest)); }
    }
    Some(canonical)
}
