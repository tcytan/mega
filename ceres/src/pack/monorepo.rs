use std::{
    path::{Component, Path, PathBuf},
    str::FromStr,
    sync::mpsc,
    vec,
};

use async_trait::async_trait;
use bytes::Bytes;

use callisto::{mega_tree, raw_blob};
use common::utils::MEGA_BRANCH_NAME;
use jupiter::storage::batch_save_model;
use jupiter::{context::Context, storage::batch_query_by_columns};
use mercury::internal::pack::encode::PackEncoder;
use venus::{
    errors::GitError,
    hash::SHA1,
    internal::{
        object::{blob::Blob, commit::Commit, tag::Tag, tree::Tree},
        pack::{
            entry::Entry,
            reference::{RefCommand, Refs},
        },
    },
    mr::MergeRequest,
    repo::Repo,
};

use crate::pack::handler::{check_head_hash, decode_for_receiver, PackHandler};

pub struct MonoRepo {
    pub context: Context,
    pub path: PathBuf,
    pub from_hash: Option<String>,
    pub to_hash: Option<String>,
}

#[async_trait]
impl PackHandler for MonoRepo {
    async fn head_hash(&self) -> (String, Vec<Refs>) {
        let refs: Vec<Refs>;
        let storage = self.context.services.mega_storage.clone();

        if self.path == PathBuf::from("/mega_mono") {
            refs = storage.get_ref("/").await.unwrap();
        } else {
            let res = storage.get_ref(self.path.to_str().unwrap()).await.unwrap();
            if !res.is_empty() {
                refs = res;
            } else {
                let target_path = self.path.clone();
                let ref_hash = storage.get_ref("/").await.unwrap()[0].ref_hash.clone();

                let commit: Commit = storage
                    .get_commit_by_hash(&Repo::empty(), &ref_hash)
                    .await
                    .unwrap()
                    .unwrap()
                    .into();
                let tree_id = commit.tree_id.to_plain_str();
                let mut tree: Tree = storage
                    .get_tree_by_hash(&Repo::empty(), &tree_id)
                    .await
                    .unwrap()
                    .unwrap()
                    .into();

                for component in target_path.components() {
                    if component != Component::RootDir {
                        let path_name = component.as_os_str().to_str().unwrap();
                        let sha1 = tree
                            .tree_items
                            .iter()
                            .find(|x| x.name == path_name)
                            .map(|x| x.id);
                        if let Some(sha1) = sha1 {
                            tree = storage
                                .get_tree_by_hash(&Repo::empty(), &sha1.to_plain_str())
                                .await
                                .unwrap()
                                .unwrap()
                                .into();
                        } else {
                            return check_head_hash(vec![]);
                        }
                    }
                }

                let c = Commit::from_tree_id(
                    tree.id,
                    vec![],
                    "This commit was generated by mega for maintain refs",
                );
                storage
                    .save_ref(
                        self.path.to_str().unwrap(),
                        &c.id.to_plain_str(),
                        &c.tree_id.to_plain_str(),
                    )
                    .await
                    .unwrap();
                storage
                    .save_mega_commits(&Repo::empty(), None, vec![c.clone()])
                    .await
                    .unwrap();

                refs = vec![Refs {
                    ref_name: MEGA_BRANCH_NAME.to_string(),
                    ref_hash: c.id.to_plain_str(),
                    ref_tree_hash: Some(c.tree_id.to_plain_str()),
                }]
            }
        }
        check_head_hash(refs)
    }

    async fn unpack(&self, pack_file: Bytes) -> Result<(), GitError> {
        let receiver = decode_for_receiver(pack_file).unwrap();

        let storage = self.context.services.mega_storage.clone();
        let mut entry_list = Vec::new();

        let mr = self.check_mr_status().await;

        // todo!() To enable mr Under monorepo, decode needs a function to get the number of commits in pack and the commit hash

        for entry in receiver {
            entry_list.push(entry);
            if entry_list.len() >= 1000 {
                storage.save_entry(&mr, entry_list).await.unwrap();
                entry_list = Vec::new();
            }
        }
        storage.save_entry(&mr, entry_list).await.unwrap();

        self.handle_parent_directory(&self.path, &mr).await.unwrap();
        Ok(())
    }

    async fn full_pack(&self) -> Result<Vec<u8>, GitError> {
        let (sender, receiver) = mpsc::channel();
        let mut writer: Vec<u8> = Vec::new();
        let repo = &Repo::empty();
        let storage = self.context.services.mega_storage.clone();
        let obj_num = storage.get_obj_count_by_repo_id(repo).await;
        let mut encoder = PackEncoder::new(obj_num, 0, &mut writer);

        for m in storage
            .get_commits_by_repo_id(repo)
            .await
            .unwrap()
            .into_iter()
        {
            let c: Commit = m.into();
            let entry: Entry = c.into();
            sender.send(entry).unwrap();
        }

        for m in storage
            .get_trees_by_repo_id(repo)
            .await
            .unwrap()
            .into_iter()
        {
            let c: Tree = m.into();
            let entry: Entry = c.into();
            sender.send(entry).unwrap();
        }

        let bids: Vec<String> = storage
            .get_blobs_by_repo_id(repo)
            .await
            .unwrap()
            .into_iter()
            .map(|b| b.blob_id)
            .collect();

        let raw_blobs = batch_query_by_columns::<raw_blob::Entity, raw_blob::Column>(
            storage.get_connection(),
            raw_blob::Column::Sha1,
            bids,
            None,
            None,
        )
        .await
        .unwrap();

        for m in raw_blobs {
            // todo handle storage type
            let c: Blob = m.into();
            let entry: Entry = c.into();
            sender.send(entry).unwrap();
        }

        for m in storage.get_tags_by_repo_id(repo).await.unwrap().into_iter() {
            let c: Tag = m.into();
            let entry: Entry = c.into();
            sender.send(entry).unwrap();
        }
        drop(sender);
        encoder.encode(receiver).unwrap();

        Ok(writer)
    }

    async fn check_commit_exist(&self, hash: &str) -> bool {
        self.context
            .services
            .mega_storage
            .get_commit_by_hash(&Repo::empty(), hash)
            .await
            .unwrap()
            .is_some()
    }

    async fn incremental_pack(
        &self,
        _want: Vec<String>,
        _have: Vec<String>,
    ) -> Result<Vec<u8>, GitError> {
        todo!()
    }

    async fn update_refs(&self, _: &RefCommand) -> Result<(), GitError> {
        //do nothing in monorepo because need mr to handle refs
        Ok(())
    }
}

impl MonoRepo {
    async fn check_mr_status(&self) -> MergeRequest {
        let storage = self.context.services.mega_storage.clone();

        let mr = storage
            .get_open_mr(self.path.to_str().unwrap())
            .await
            .unwrap();
        if let Some(mr) = mr {
            mr
        } else {
            let mr = MergeRequest {
                path: self.path.to_str().unwrap().to_owned(),
                from_hash: self.from_hash.clone().unwrap(),
                to_hash: self.to_hash.clone().unwrap(),
                ..Default::default()
            };
            storage.save_mr(mr.clone()).await.unwrap();
            mr
        }
    }

    async fn handle_parent_directory(
        &self,
        mut path: &Path,
        mr: &MergeRequest,
    ) -> Result<(), GitError> {
        let storage = self.context.services.mega_storage.clone();
        let refs = &storage.get_ref("/").await.unwrap()[0];

        let mut target_name = path.file_name().unwrap().to_str().unwrap();
        let mut target_hash = SHA1::from_str(&refs.ref_tree_hash.clone().unwrap()).unwrap();

        let mut save_models: Vec<mega_tree::ActiveModel> = Vec::new();

        while let Some(parent) = path.parent() {
            let model = storage
                .get_tree_by_path(parent.to_str().unwrap(), &refs.ref_hash)
                .await
                .unwrap();
            if let Some(model) = model {
                let mut p_tree: Tree = model.into();
                let index = p_tree.tree_items.iter().position(|x| x.name == target_name);
                if let Some(index) = index {
                    p_tree.tree_items[index].id = target_hash;
                    let new_p_tree = Tree::from_tree_items(p_tree.tree_items).unwrap();

                    if parent.parent().is_some() {
                        target_name = parent.file_name().unwrap().to_str().unwrap();
                        target_hash = new_p_tree.id;
                    } else {
                        target_name = "root";
                    }

                    let mut model: mega_tree::Model = new_p_tree.into();
                    model.mr_id = mr.id;
                    model.status = mr.status;
                    model.full_path = parent.to_str().unwrap().to_owned();
                    model.name = target_name.to_owned();
                    let a_model = model.into();
                    save_models.push(a_model);
                } else {
                    return Err(GitError::ConversionError("Can't find child.".to_string()));
                }
            } else {
                return Err(GitError::ConversionError(
                    "Can't find parent tree.".to_string(),
                ));
            }
            path = parent;
        }

        batch_save_model(storage.get_connection(), save_models)
            .await
            .unwrap();

        Ok(())
    }
}
