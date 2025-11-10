use crate::{
    ctx::{AppCtx, AppError},
    file::File,
    filter::textish_filename_regex,
};

type Result<T> = std::result::Result<T, AppError>;

// ---------------- Business logic layer ----------------

pub async fn find_content(ctx: &AppCtx) -> Result<Vec<File>> {
    if !ctx.root_dir().exists() {
        return Err(AppError::Msg("Root directory not found".to_string()));
    }

    let content_dir = ctx.root_dir().join("content");

    if !content_dir.exists() {
        return Err(AppError::Msg("Content directory not found".to_string()));
    }

    let fs = ctx.file_service();
    let (folder, _report) = fs.scan_with_report_and_filters(
        content_dir.as_path(),
        None,
        Some(&textish_filename_regex()?),
    )?;

    let files: Vec<File> = folder.files().iter().map(|f| f.as_ref().clone()).collect();

    Ok(files)
}
