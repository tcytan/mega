use std::{
    collections::HashSet,
    io::Cursor,
    pin::Pin,
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc::{self, Receiver},
    },
};

use async_trait::async_trait;
use bytes::Bytes;
use futures::Stream;
use tokio_stream::wrappers::ReceiverStream;

use callisto::raw_blob;
use common::{config::PackConfig, errors::MegaError, utils::ZERO_ID};
use mercury::internal::pack::Pack;
use mercury::{
    errors::GitError,
    internal::{
        object::{
            blob::Blob,
            tree::{Tree, TreeItemMode},
        },
        pack::entry::Entry,
    },
};
use venus::import_repo::import_refs::{RefCommand, Refs};

#[async_trait]
pub trait PackHandler: Send + Sync {
    async fn head_hash(&self) -> (String, Vec<Refs>);

    fn find_head_hash(&self, refs: Vec<Refs>) -> (String, Vec<Refs>) {
        let mut head_hash = ZERO_ID.to_string();
        for git_ref in refs.iter() {
            if git_ref.default_branch {
                head_hash.clone_from(&git_ref.ref_hash);
            }
        }
        (head_hash, refs)
    }

    async fn unpack(&self, pack_config: &PackConfig, pack_file: Bytes) -> Result<(), GitError> {
        // #[cfg(debug_assertions)]
        // {
        //     let datetime = chrono::Utc::now().naive_utc();
        //     let path = format!("{}.pack", datetime);
        //     let mut output = std::fs::File::create(path).unwrap();
        //     output.write_all(&pack_file).unwrap();
        // }
        let (sender, receiver) = mpsc::channel();
        let p = Pack::new(
            None,
            Some(1024 * 1024 * 1024 * pack_config.pack_decode_mem_size),
            Some(pack_config.pack_decode_cache_path.clone()),
            pack_config.clean_cache_after_decode,
        );

        p.decode_async(Cursor::new(pack_file), sender); //Pack moved here

        self.save_entry(receiver).await
    }

    async fn unpack_stream(
        &self,
        pack_config: &PackConfig,
        stream: Pin<Box<dyn Stream<Item = Result<Bytes, axum::Error>> + Send>>,
    ) -> Result<Receiver<Entry>, GitError> {
        let (sender, receiver) = std::sync::mpsc::channel();
        let p = Pack::new(
            None,
            Some(1024 * 1024 * 1024 * pack_config.pack_decode_mem_size),
            Some(pack_config.pack_decode_cache_path.clone()),
            pack_config.clean_cache_after_decode,
        );
        tokio::spawn(async move {
            p.decode_stream(stream, sender).await;
        });
        Ok(receiver)
    }

    async fn save_entry(&self, rx: Receiver<Entry>) -> Result<(), GitError>;

    /// Asynchronously retrieves the full pack data for the specified repository path.
    /// This function collects commits and nodes from the storage and packs them into
    /// a single binary vector. There is no need to build the entire tree; the function
    /// only sends all the data related to this repository.
    ///
    /// # Returns
    /// * `Result<Vec<u8>, GitError>` - The packed binary data as a vector of bytes.
    ///
    async fn full_pack(&self) -> Result<ReceiverStream<Vec<u8>>, GitError>;

    async fn incremental_pack(
        &self,
        want: Vec<String>,
        have: Vec<String>,
    ) -> Result<ReceiverStream<Vec<u8>>, GitError>;

    async fn traverse_for_count(
        &self,
        tree: Tree,
        exist_objs: &HashSet<String>,
        counted_obj: &mut HashSet<String>,
        obj_num: &AtomicUsize,
    ) {
        let mut search_tree_ids = vec![];
        let mut search_blob_ids = vec![];
        for item in &tree.tree_items {
            let hash = item.id.to_plain_str();
            if !exist_objs.contains(&hash) && counted_obj.insert(hash.clone()) {
                if item.mode == TreeItemMode::Tree {
                    search_tree_ids.push(hash.clone())
                } else {
                    search_blob_ids.push(hash.clone());
                }
            }
        }
        obj_num.fetch_add(search_blob_ids.len(), Ordering::SeqCst);
        let trees = self.get_trees_by_hashes(search_tree_ids).await.unwrap();
        for t in trees {
            self.traverse_for_count(t, exist_objs, counted_obj, obj_num)
                .await;
        }
        obj_num.fetch_add(1, Ordering::SeqCst);
    }

    /// Traverse a tree structure asynchronously.
    ///
    /// This function traverses a given tree, keeps track of processed objects, and optionally sends
    /// traversal data to a provided sender. The function will:
    /// 1. Traverse the tree and calculate the quantities of tree and blob items.
    /// 2. If a sender is provided, send blob and tree data via the sender.
    ///
    /// # Parameters
    /// - `tree`: The tree structure to traverse.
    /// - `exist_objs`: A mutable reference to a set containing already processed object IDs.
    /// - `sender`: An optional sender for sending traversal data.
    ///
    /// # Details
    /// - The function processes tree items, distinguishing between tree and blob items.
    /// - It collects IDs of items that have not been processed yet.
    /// - It retrieves and sends blob data if a sender is provided.
    /// - It recursively traverses sub-trees.
    /// - It sends the entire tree data if a sender is provided.
    async fn traverse(
        &self,
        tree: Tree,
        exist_objs: &mut HashSet<String>,
        sender: Option<&tokio::sync::mpsc::Sender<Entry>>,
    ) {
        let mut search_tree_ids = vec![];
        let mut search_blob_ids = vec![];

        for item in &tree.tree_items {
            let hash = item.id.to_plain_str();
            if exist_objs.insert(hash.clone()) {
                if item.mode == TreeItemMode::Tree {
                    search_tree_ids.push(hash);
                } else {
                    search_blob_ids.push(hash);
                }
            }
        }

        if let Some(sender) = sender {
            let blobs = self.get_blobs_by_hashes(search_blob_ids).await.unwrap();
            for b in blobs {
                let blob: Blob = b.into();
                sender.send(blob.into()).await.unwrap();
            }
        }

        let trees = self.get_trees_by_hashes(search_tree_ids).await.unwrap();
        for t in trees {
            self.traverse(t, exist_objs, sender).await;
        }

        if let Some(sender) = sender {
            sender.send(tree.into()).await.unwrap();
        }
    }

    async fn get_trees_by_hashes(&self, hashes: Vec<String>) -> Result<Vec<Tree>, MegaError>;

    async fn get_blobs_by_hashes(
        &self,
        hashes: Vec<String>,
    ) -> Result<Vec<raw_blob::Model>, MegaError>;

    async fn update_refs(&self, refs: &RefCommand) -> Result<(), GitError>;

    async fn check_commit_exist(&self, hash: &str) -> bool;

    async fn check_default_branch(&self) -> bool;
}
