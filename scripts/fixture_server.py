#!/usr/bin/env python3
"""A tiny Subsonic server for local checks.

The demo server is slow and sometimes gone; this answers the handful of
endpoints the interface uses, with three albums and a real cover image, so a
check can run in seconds and offline.

    python3 scripts/fixture_server.py [port]

Prints the port it bound to on the first line, so a caller can read it.
"""

import base64
import hashlib
import json
import struct
import sys
import zlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse


def png(width=96, height=96):
    """A cover-shaped gradient, so chafa has something with structure."""
    raw = b""
    for y in range(height):
        raw += b"\x00"
        for x in range(width):
            r = int(255 * x / width)
            g = int(255 * y / height)
            b = 255 - int(128 * (x + y) / (width + height))
            raw += bytes((r, g, b))

    def chunk(kind, data):
        body = kind + data
        return (
            struct.pack(">I", len(data))
            + body
            + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)
        )

    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(raw))
        + chunk(b"IEND", b"")
    )


COVER = png()
ALBUMS = [
    {
        "id": f"al-{i}",
        "name": name,
        "artist": artist,
        "year": 2000 + i,
        "songCount": 3,
        "duration": 600,
        "coverArt": f"al-{i}",
        "created": "2020-01-01T00:00:00Z",
    }
    for i, (name, artist) in enumerate(
        [("Fixture One", "Test Artist"), ("Fixture Two", "Other Band"), ("Third", "Nobody")]
    )
]


def album_payload(album_id):
    album = next((a for a in ALBUMS if a["id"] == album_id), ALBUMS[0])
    return {
        **album,
        "song": [
            {
                "id": f"{album['id']}-s{n}",
                "title": f"Track {n + 1}",
                "artist": album["artist"],
                "album": album["name"],
                "duration": 200,
                "track": n + 1,
                "suffix": "flac",
                "bitRate": 900,
            }
            for n in range(3)
        ],
    }


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, format, *args):
        pass

    def _send(self, body, content_type="application/json"):
        if isinstance(body, (dict, list)):
            body = json.dumps(body).encode()
        self.send_response(200)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        parsed = urlparse(self.path)
        endpoint = parsed.path.rsplit("/", 1)[-1].replace(".view", "")
        query = parse_qs(parsed.query)

        if endpoint == "getCoverArt":
            return self._send(COVER, "image/png")
        if endpoint == "stream":
            return self._send(b"\0" * 1024, "audio/flac")

        response: dict = {"status": "ok", "version": "1.16.1", "type": "fixture", "serverVersion": "1"}
        if endpoint == "getAlbumList2":
            response["albumList2"] = {"album": ALBUMS}
        elif endpoint == "getAlbum":
            response["album"] = album_payload(query.get("id", ["al-0"])[0])
        elif endpoint == "search3":
            response["searchResult3"] = {"album": ALBUMS, "song": album_payload("al-0")["song"]}
        elif endpoint == "getArtists":
            response["artists"] = {
                "index": [
                    {
                        "name": "T",
                        "artist": [{"id": "ar-1", "name": "Test Artist", "albumCount": 3}],
                    }
                ]
            }
        elif endpoint == "getTopSongs":
            response["topSongs"] = {
                "song": [
                    {
                        "id": f"top-{n}",
                        "title": f"Top Track {n + 1}",
                        "artist": "Test Artist",
                        "album": "Fixture One",
                        "duration": 180 + n,
                        "suffix": "flac",
                        "bitRate": 900,
                    }
                    for n in range(4)
                ]
            }
        elif endpoint == "getArtistInfo2":
            response["artistInfo2"] = {
                "biography": (
                    "A fixture artist, here so the biography pane has something "
                    "to wrap. The text is long enough to need several lines in a "
                    "narrow column, which is the case the wrap is for."
                ),
                "similarArtist": [
                    {"id": "ar-2", "name": "Similar One", "albumCount": 2},
                    {"id": "ar-3", "name": "Similar Two", "albumCount": 1},
                ],
            }
        elif endpoint == "getArtist":
            response["artist"] = {"id": "ar-1", "name": "Test Artist", "album": ALBUMS}
        elif endpoint == "getStarred2":
            response["starred2"] = {"album": [], "song": []}
        elif endpoint == "getPlaylists":
            response["playlists"] = {
                "playlist": [
                    {"id": "pl-1", "name": "Fixture List", "songCount": 3, "duration": 600}
                ]
            }
        elif endpoint == "getPlaylist":
            response["playlist"] = {
                "id": "pl-1",
                "name": "Fixture List",
                "songCount": 3,
                "entry": album_payload("al-0")["song"],
            }
        elif endpoint in ("star", "unstar", "scrobble"):
            pass
        self._send({"subsonic-response": response})


def main():
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 0
    server = ThreadingHTTPServer(("127.0.0.1", port), Handler)
    print(server.server_address[1], flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
