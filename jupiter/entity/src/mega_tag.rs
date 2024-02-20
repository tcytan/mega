//! `SeaORM` Entity. Generated by sea-orm-codegen 0.11.3

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "mega_tag")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: i64,
    #[sea_orm(unique)]
    pub tag_id: String,
    pub object_id: String,
    pub object_type: Option<String>,
    #[sea_orm(column_type = "Text")]
    pub tag_name: String,
    #[sea_orm(column_type = "Text")]
    pub tagger: String,
    #[sea_orm(column_type = "Text")]
    pub message: String,
    pub created_at: DateTime,
    pub updated_at: DateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
