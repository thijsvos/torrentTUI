//! The engine half of the app: `torrent` owns the librqbit session and the
//! command loop, `watch` handles the watch-folder cleanup librqbit does not do.

pub mod torrent;
pub mod watch;
