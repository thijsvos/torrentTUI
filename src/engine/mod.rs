//! The engine half of the app: `torrent` owns the librqbit session and the
//! command loop, `watch` handles the watch-folder cleanup librqbit does not do,
//! `leftovers` removes what is left of unselected files when a torrent is
//! deleted with its files kept, and `health_capture` overhears the tracker and
//! UPnP outcomes librqbit logs but never exposes through its API.

pub mod health_capture;
pub mod leftovers;
pub mod torrent;
pub mod watch;
