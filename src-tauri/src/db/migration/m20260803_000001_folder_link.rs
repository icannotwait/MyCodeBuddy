use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // folder_link: a directory the user explicitly symlinked into a
        // workspace folder, turning one root into a multi-folder workspace the
        // agent CLI (whose cwd is that root) can actually traverse.
        //
        // This table is also the *authorization* record: the workspace path
        // guard only follows a symlink out of the root when a row here says the
        // user asked for it, so a checked-in `secrets -> ~/.ssh` in some cloned
        // repo stays unreadable. Hard-deleted — nothing references a link, and a
        // tombstone would keep granting access after the user unlinked.
        manager
            .create_table(
                Table::create()
                    .table(FolderLink::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(FolderLink::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(FolderLink::FolderId).integer().not_null())
                    // Name of the symlink inside the root folder.
                    .col(ColumnDef::new(FolderLink::Name).text().not_null())
                    // Absolute path of the linked directory, stored verbatim so
                    // the UI can show what the user picked. Resolved (and
                    // re-resolved) at use time rather than canonicalized here.
                    .col(ColumnDef::new(FolderLink::TargetPath).text().not_null())
                    .col(
                        ColumnDef::new(FolderLink::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(FolderLink::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_folder_link_folder_id")
                    .table(FolderLink::Table)
                    .col(FolderLink::FolderId)
                    .to_owned(),
            )
            .await?;

        // One name per root: the symlink itself is a filesystem entry, so two
        // rows claiming the same name could not both exist on disk anyway.
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_folder_link_folder_id_name")
                    .table(FolderLink::Table)
                    .col(FolderLink::FolderId)
                    .col(FolderLink::Name)
                    .unique()
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(FolderLink::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum FolderLink {
    Table,
    Id,
    FolderId,
    Name,
    TargetPath,
    CreatedAt,
    UpdatedAt,
}
