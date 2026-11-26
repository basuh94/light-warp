use warp_util::local_or_remote_path::LocalOrRemotePath;
pub use warp_util::path::*;
use warpui::{AppContext, SingletonEntity};

use crate::remote_server::manager::RemoteServerManager;

/// Returns the display name of a local or remote path, prefixed with the
/// host label for remote paths.
pub fn display_name_with_host(path: &LocalOrRemotePath, ctx: &AppContext) -> String {
    let name = path.display_name();
    match path {
        LocalOrRemotePath::Local(_) => name.to_string(),
        LocalOrRemotePath::Remote(remote) => {
            let host_label = RemoteServerManager::as_ref(ctx)
                .host_label(&remote.host_id)
                .unwrap_or(light_i18n::tr("Remote host"));
            format!("{host_label}:{name}")
        }
    }
}

/// Returns the display path of a local or remote path,
/// prefixed with the host label for remote paths.
///
/// When `abbreviate_home` is true, local paths under the user's home directory
/// are abbreviated with a `~/` prefix. The flag is ignored for remote paths,
/// whose home directory lives on a different machine.
pub fn display_path_with_host(
    path: &LocalOrRemotePath,
    abbreviate_home: bool,
    ctx: &AppContext,
) -> String {
    match path {
        LocalOrRemotePath::Local(local_path) => {
            if abbreviate_home {
                dirs::home_dir()
                    .and_then(|home| local_path.strip_prefix(&home).ok())
                    .map(|relative| format!("~/{}", relative.display()))
                    .unwrap_or_else(|| local_path.display().to_string())
            } else {
                path.display_path()
            }
        }
        LocalOrRemotePath::Remote(remote) => {
            let host_label = RemoteServerManager::as_ref(ctx)
                .host_label(&remote.host_id)
                .unwrap_or(light_i18n::tr("Remote host"));
            format!("{host_label}:{}", path.display_path())
        }
    }
}
