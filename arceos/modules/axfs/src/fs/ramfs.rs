//! In-memory RAM filesystem implementing [`axfs_vfs`].

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;

use axfs_vfs::{
    impl_vfs_dir_default, impl_vfs_non_dir_default, VfsDirEntry, VfsError, VfsNodeAttr,
    VfsNodeOps, VfsNodeRef, VfsNodeType, VfsOps, VfsResult,
};
use axsync::Mutex;

/// A RAM filesystem that implements [`axfs_vfs::VfsOps`].
pub struct RamFileSystem {
    parent: Mutex<Option<VfsNodeRef>>,
    root: Arc<DirNode>,
}

impl RamFileSystem {
    /// Creates a new empty RAM filesystem.
    pub fn new() -> Self {
        Self {
            parent: Mutex::new(None),
            root: DirNode::new(None),
        }
    }
}

impl Default for RamFileSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl VfsOps for RamFileSystem {
    fn mount(&self, _path: &str, mount_point: VfsNodeRef) -> VfsResult {
        let mut guard = self.parent.lock();
        *guard = mount_point.parent();
        self.root.set_parent(guard.as_ref());
        Ok(())
    }

    fn root_dir(&self) -> VfsNodeRef {
        self.root.clone()
    }
}

/// A file node stored in memory.
pub struct FileNode {
    content: Mutex<Vec<u8>>,
}

impl FileNode {
    const fn new() -> Self {
        Self {
            content: Mutex::new(Vec::new()),
        }
    }
}

impl VfsNodeOps for FileNode {
    impl_vfs_non_dir_default! {}

    fn get_attr(&self) -> VfsResult<VfsNodeAttr> {
        Ok(VfsNodeAttr::new_file(self.content.lock().len() as u64, 0))
    }

    fn truncate(&self, size: u64) -> VfsResult {
        let mut content = self.content.lock();
        let size = size as usize;
        if size < content.len() {
            content.truncate(size);
        } else {
            content.resize(size, 0);
        }
        Ok(())
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> VfsResult<usize> {
        let content = self.content.lock();
        let start = content.len().min(offset as usize);
        let end = content.len().min(start + buf.len());
        let src = &content[start..end];
        buf[..src.len()].copy_from_slice(src);
        Ok(src.len())
    }

    fn write_at(&self, offset: u64, buf: &[u8]) -> VfsResult<usize> {
        let offset = offset as usize;
        let mut content = self.content.lock();
        if offset + buf.len() > content.len() {
            content.resize(offset + buf.len(), 0);
        }
        content[offset..offset + buf.len()].copy_from_slice(buf);
        Ok(buf.len())
    }
}

/// A directory node stored in memory.
pub struct DirNode {
    this: Weak<DirNode>,
    parent: Mutex<Weak<dyn VfsNodeOps>>,
    children: Mutex<BTreeMap<String, VfsNodeRef>>,
}

impl DirNode {
    fn new(parent: Option<Weak<dyn VfsNodeOps>>) -> Arc<Self> {
        Arc::new_cyclic(|this| Self {
            this: this.clone(),
            parent: Mutex::new(parent.unwrap_or_else(|| Weak::<Self>::new())),
            children: Mutex::new(BTreeMap::new()),
        })
    }

    fn set_parent(&self, parent: Option<&VfsNodeRef>) {
        *self.parent.lock() = parent.map_or(Weak::<Self>::new() as _, Arc::downgrade);
    }

    fn child_node(&self, name: &str) -> VfsResult<VfsNodeRef> {
        match name {
            "" | "." => Ok(self.this.upgrade().ok_or(VfsError::NotFound)?),
            ".." => self.parent().ok_or(VfsError::NotFound),
            _ => self
                .children
                .lock()
                .get(name)
                .cloned()
                .ok_or(VfsError::NotFound),
        }
    }

    fn create_node(&self, name: &str, ty: VfsNodeType) -> VfsResult {
        let mut children = self.children.lock();
        if children.contains_key(name) {
            return Err(VfsError::AlreadyExists);
        }
        let node: VfsNodeRef = match ty {
            VfsNodeType::File => Arc::new(FileNode::new()),
            VfsNodeType::Dir => Self::new(Some(self.this.clone())),
            _ => return Err(VfsError::Unsupported),
        };
        children.insert(name.into(), node);
        Ok(())
    }

    fn remove_node(&self, name: &str) -> VfsResult {
        let mut children = self.children.lock();
        let node = children.get(name).ok_or(VfsError::NotFound)?;
        if let Some(dir) = node.as_any().downcast_ref::<DirNode>() {
            if !dir.children.lock().is_empty() {
                return Err(VfsError::DirectoryNotEmpty);
            }
        }
        children.remove(name);
        Ok(())
    }
}

impl VfsNodeOps for DirNode {
    impl_vfs_dir_default! {}

    fn get_attr(&self) -> VfsResult<VfsNodeAttr> {
        Ok(VfsNodeAttr::new_dir(4096, 0))
    }

    fn parent(&self) -> Option<VfsNodeRef> {
        self.parent.lock().upgrade()
    }

    fn lookup(self: Arc<Self>, path: &str) -> VfsResult<VfsNodeRef> {
        let (name, rest) = split_path(path);
        let node = self.child_node(name)?;
        if let Some(rest) = rest {
            node.lookup(rest)
        } else {
            Ok(node)
        }
    }

    fn read_dir(&self, start_idx: usize, dirents: &mut [VfsDirEntry]) -> VfsResult<usize> {
        let children = self.children.lock();
        let mut iter = children.iter().skip(start_idx.max(2) - 2);
        for (i, ent) in dirents.iter_mut().enumerate() {
            match i + start_idx {
                0 => *ent = VfsDirEntry::new(".", VfsNodeType::Dir),
                1 => *ent = VfsDirEntry::new("..", VfsNodeType::Dir),
                _ => {
                    if let Some((name, node)) = iter.next() {
                        *ent = VfsDirEntry::new(name, node.get_attr().unwrap().file_type());
                    } else {
                        return Ok(i);
                    }
                }
            }
        }
        Ok(dirents.len())
    }

    fn create(&self, path: &str, ty: VfsNodeType) -> VfsResult {
        let (name, rest) = split_path(path);
        if let Some(rest) = rest {
            self.child_node(name)?.create(rest, ty)
        } else if name.is_empty() || name == "." || name == ".." {
            Ok(())
        } else {
            self.create_node(name, ty)
        }
    }

    fn remove(&self, path: &str) -> VfsResult {
        let (name, rest) = split_path(path);
        if let Some(rest) = rest {
            self.child_node(name)?.remove(rest)
        } else if name.is_empty() || name == "." || name == ".." {
            Err(VfsError::InvalidInput)
        } else {
            self.remove_node(name)
        }
    }

    fn rename(&self, src_path: &str, dst_path: &str) -> VfsResult {
        let (src_dir_path, src_name) = split_parent(src_path)?;
        let (dst_dir_path, dst_name) = split_parent(dst_path)?;
        let root = self.this.upgrade().ok_or(VfsError::NotFound)?;
        let src_node = root.clone().lookup(src_dir_path)?;
        let dst_node = root.lookup(dst_dir_path)?;
        let src_dir = src_node
            .as_any()
            .downcast_ref::<DirNode>()
            .ok_or(VfsError::NotADirectory)?;
        let dst_dir = dst_node
            .as_any()
            .downcast_ref::<DirNode>()
            .ok_or(VfsError::NotADirectory)?;

        if !core::ptr::eq(src_dir, dst_dir) {
            return Err(VfsError::Unsupported);
        }

        let mut children = src_dir.children.lock();
        if src_name == dst_name {
            return if children.contains_key(src_name) {
                Ok(())
            } else {
                Err(VfsError::NotFound)
            };
        }
        if let Some(existing) = children.get(dst_name) {
            if let Some(dir) = existing.as_any().downcast_ref::<DirNode>() {
                if !dir.children.lock().is_empty() {
                    return Err(VfsError::DirectoryNotEmpty);
                }
            }
        }
        let node = children.remove(src_name).ok_or(VfsError::NotFound)?;
        children.insert(dst_name.into(), node);
        Ok(())
    }
}

fn split_path(path: &str) -> (&str, Option<&str>) {
    let trimmed = path.trim_start_matches('/');
    trimmed.find('/').map_or((trimmed, None), |n| {
        (&trimmed[..n], Some(&trimmed[n + 1..]))
    })
}

fn split_parent(path: &str) -> VfsResult<(&str, &str)> {
    let trimmed = path.trim_matches('/');
    if trimmed.is_empty() {
        return Err(VfsError::InvalidInput);
    }
    let (parent, name) = trimmed.rsplit_once('/').unwrap_or((".", trimmed));
    if name == "." || name == ".." {
        return Err(VfsError::InvalidInput);
    }
    Ok((parent, name))
}
