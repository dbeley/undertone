//! Measures cover fetching and encoding separately against a real server.
//!
//! ```text
//! UNDERTONE_PASSWORD=demo cargo run --release --example fetch_bench -- \
//!     https://demo.navidrome.org demo
//! ```

use std::sync::Arc;
use std::time::Instant;

use undertone::art::SubsonicImageSource;
use undertone::render::{
    ArtProtocol, ArtRenderer, ChafaRenderer, ImageCache, ImageSource, RenderRequest,
};
use undertone::subsonic::{AlbumListQuery, AlbumListType, SubsonicClient, SubsonicConfig};

fn main() {
    let mut args = std::env::args().skip(1);
    let server = args
        .next()
        .unwrap_or_else(|| "https://demo.navidrome.org".into());
    let user = args.next().unwrap_or_else(|| "demo".into());
    let password = std::env::var("UNDERTONE_PASSWORD").unwrap_or_else(|_| "demo".into());

    let client = SubsonicClient::new(SubsonicConfig {
        server,
        username: user,
        password,
    });

    let albums = client
        .album_list(&AlbumListQuery {
            kind: AlbumListType::Random,
            size: 14,
            ..Default::default()
        })
        .expect("album list");
    let covers: Vec<String> = albums
        .iter()
        .filter_map(|album| album.cover_art.clone())
        .collect();
    println!("{} albums, {} with art", albums.len(), covers.len());

    let source = Arc::new(SubsonicImageSource::new(client));
    let dir = std::env::temp_dir().join(format!("undertone-fetchbench-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);

    // Serial.
    let start = Instant::now();
    let mut serial_bytes = 0;
    for cover in &covers {
        if let Ok(bytes) = source.fetch(cover, 200) {
            serial_bytes += bytes.len();
        }
    }
    let serial = start.elapsed();
    println!(
        "serial fetch:    {serial:>8.1?} for {} covers ({serial_bytes} bytes)",
        covers.len()
    );

    // Concurrent, 4 at a time, same shape as the worker.
    let start = Instant::now();
    let mut concurrent_bytes = 0;
    for chunk in covers.chunks(4) {
        let results: Vec<_> = std::thread::scope(|scope| {
            let handles: Vec<_> = chunk
                .iter()
                .map(|cover| {
                    let source = source.as_ref();
                    scope.spawn(move || source.fetch(cover, 200))
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        for result in results {
            if let Ok(bytes) = result {
                concurrent_bytes += bytes.len();
            }
        }
    }
    let concurrent = start.elapsed();
    println!(
        "4-way fetch:     {concurrent:>8.1?} for {} covers ({concurrent_bytes} bytes)",
        covers.len()
    );

    // Encode from already-fetched bytes on disk.
    let images = ImageCache::new(dir.join("img")).expect("image cache");
    for (i, cover) in covers.iter().enumerate() {
        if let Ok(bytes) = source.fetch(cover, 200) {
            let _ = images.store(&format!("k{i}"), &bytes);
        }
    }
    let renderer = ChafaRenderer::with_default_binary(ArtProtocol::Sixel);
    let requests: Vec<RenderRequest> = (0..covers.len())
        .map(|i| RenderRequest::new(images.path_for(&format!("k{i}")), 4, 2, 16))
        .collect();

    let start = Instant::now();
    let mut blob_bytes = 0;
    for chunk in requests.chunks(8) {
        if let Ok(blobs) = renderer.render(chunk) {
            blob_bytes += blobs.iter().map(Vec::len).sum::<usize>();
        }
    }
    println!(
        "encode:          {:>8.1?} for {} covers ({blob_bytes} bytes)",
        start.elapsed(),
        covers.len()
    );

    let _ = std::fs::remove_dir_all(&dir);
}
