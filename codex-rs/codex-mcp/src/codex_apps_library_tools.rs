pub const LIBRARY_SEARCH_FILE_TOOL_NAME: &str = "library_search_file";
pub const LIBRARY_DOWNLOAD_FILE_TOOL_NAME: &str = "library_download_file";
pub const LIBRARY_CREATE_FILE_TOOL_NAME: &str = "library_create_file";

pub fn is_codex_apps_library_tool(tool_name: &str) -> bool {
    matches!(
        tool_name,
        LIBRARY_SEARCH_FILE_TOOL_NAME
            | LIBRARY_DOWNLOAD_FILE_TOOL_NAME
            | LIBRARY_CREATE_FILE_TOOL_NAME
    )
}
