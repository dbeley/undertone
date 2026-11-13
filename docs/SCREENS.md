# Screens

Rendered by `scripts/screens.py` from the real binary driving a pty
against `scripts/fixture_server.py`, so what is written here is what the
interface draws. Regenerate with
`python3 scripts/screens.py ./target/release/undertone`.

Covers are characters here because the fixture runs with `--art=symbols`;
on a terminal with sixel or kitty support they are images in the same
places. See `docs/COMPATIBILITY.md`.

## The library

Album list, with the rail, the list's own header and the three-column bar.

```
  Albums   1│ Queue                                                                                    0
  Artists  2│ ┌ Queue — 0 tracks · shuffle off · repeat off ───────────────────────────────────────────┐
  Starred  3│ │                                                                                        │
  Later    4│ │                                                                                        │
  Lists    5│ │                                                                                        │
▸ Queue    6│ │                                                                                        │
            │ │                                                                                        │
            │ │                                                                                        │
            │ │                                                                                        │
            │ │                                                                                        │
            │ │                                                                                        │
            │ │                                                                                        │
            │ │                                                                                        │
            │ │                                                                                        │
            │ │                                                                                        │
            │ │                                                                                        │
            │ │                                                                                        │
            │ │                                                                                        │
            │ │                                                                                        │
            │ │                                                                                        │
            │ │                                                                                        │
            │ │                                                                                        │
            │ │                                                                                        │
            │ │                                                                                        │
            │ └────────────────────────────────────────────────────────────────────────────────────────┘
────────────────────────────────────────────────────────────────────────────────────────────────────────
nothing playing                    │repeat off · shuffle off        │next
Enter on an album to start         │Enter play · > skip · h back    │end of queue
                                   │                                │
                                   │                                │
```

## An album

The album's cover and its tracks; the bar keeps playing state.

```
  Albums   1│ Queue                                                                                    0
  Artists  2│ ┌ Queue — 0 tracks · shuffle off · repeat off ───────────────────────────────────────────┐
  Starred  3│ │                                                                                        │
  Later    4│ │                                                                                        │
  Lists    5│ │                                                                                        │
▸ Queue    6│ │                                                                                        │
            │ │                                                                                        │
            │ │                                                                                        │
            │ │                                                                                        │
            │ │                                                                                        │
            │ │                                                                                        │
            │ │                                                                                        │
            │ │                                                                                        │
            │ │                                                                                        │
            │ │                                                                                        │
            │ │                                                                                        │
            │ │                                                                                        │
            │ │                                                                                        │
            │ │                                                                                        │
            │ │                                                                                        │
            │ │                                                                                        │
            │ │                                                                                        │
            │ │                                                                                        │
            │ │                                                                                        │
            │ └────────────────────────────────────────────────────────────────────────────────────────┘
────────────────────────────────────────────────────────────────────────────────────────────────────────
nothing playing                    │repeat off · shuffle off        │next
Enter on an album to start         │Enter play · > skip · h back    │end of queue
                                   │                                │
                                   │                                │
```

## Search

One flat result list: albums first, then songs, each row labelled.

```
  Albums   1│ Search · fixture                                                                     1 / 6
  Artists  2│ album Fixture One  Test Artist
  Starred  3│ album Fixture Two  Other Band
  Later    4│ album Third  Nobody
  Lists    5│ song  Track 1  Test Artist
  Queue    6│ song  Track 2  Test Artist
            │ song  Track 3  Test Artist
            │
            │
            │
            │
            │
            │
            │
            │
            │
            │
            │
            │
            │
            │
            │
            │
            │
            │
────────────────────────────────────────────────────────────────────────────────────────────────────────
nothing playing                    │6 results for “fixture”         │next
Enter on an album to start         │/ edit · Enter open · h back    │end of queue
                                   │                                │
                                   │                                │
```

## Playlists

The server's playlists, with their size.

```
  Albums   1│ Playlists                                                                                1
  Artists  2│ Fixture List  3 tracks
  Starred  3│
  Later    4│
▸ Lists    5│
  Queue    6│
            │
            │
            │
            │
            │
            │
            │
            │
            │
            │
            │
            │
            │
            │
            │
            │
            │
            │
            │
────────────────────────────────────────────────────────────────────────────────────────────────────────
nothing playing                    │6 results for “fixture”         │next
Enter on an album to start         │Enter open · h back             │end of queue
                                   │                                │
                                   │                                │
```

## Artists

Artists as a list; `w` turns this into a wall of covers.

```
  Albums   1│ Artists · most albums                                                                1 / 1
▸ Artists  2│ ▸      Test Artist
  Starred  3│        3 albums
  Later    4│
  Lists    5│
  Queue    6│
            │
            │
            │
            │
            │
            │
            │
            │
            │
            │
            │
            │
            │
            │
            │
            │
            │
            │
            │
────────────────────────────────────────────────────────────────────────────────────────────────────────
nothing playing                    │repeat off · shuffle off        │next
Enter on an album to start         │S sort · Enter open · h back    │end of queue
                                   │                                │
                                   │                                │
```

## An artist

Three panes: albums, similar artists, biography. Tab moves between them.

```
  Albums   1│ Artist                                                                         Test Artist
▸ Artists  2│ albums                                        │ similar
  Starred  3│        Fixture One                            │ Similar One
  Later    4│ yyy _y Test Artist · 2000                     │ Similar Two
  Lists    5│     y~ 3 tracks                               │
  Queue    6│        Fixture Two                            │
            │ yyy _y Other Band · 2001                      │
            │     y~ 3 tracks                               │
            │        Third                                  │
            │ yyy _y Nobody · 2002                          │
            │     y~ 3 tracks                               │
            │                                               │
            │                                               │
            │                                               │ ──────────────────────────────────────────
            │                                               │ biography
            │                                               │ A fixture artist, here so the biography
            │                                               │ pane has something to wrap. The text is
            │                                               │ long enough to need several lines in a
            │                                               │ narrow column, which is the case the wrap
            │                                               │ is for.
            │                                               │
            │                                               │
            │                                               │
            │                                               │
            │                                               │
────────────────────────────────────────────────────────────────────────────────────────────────────────
nothing playing                    │repeat off · shuffle off        │next
Enter on an album to start         │Tab pane · Enter open · * star ·│end of queue
                                   │h back                          │
                                   │                                │
```

## The queue

What plays next; Enter starts from the cursor.

```
  Albums   1│ Queue                                                                                    0
  Artists  2│ ┌ Queue — 0 tracks · shuffle off · repeat off ───────────────────────────────────────────┐
  Starred  3│ │                                                                                        │
  Later    4│ │                                                                                        │
  Lists    5│ │                                                                                        │
▸ Queue    6│ │                                                                                        │
            │ │                                                                                        │
            │ │                                                                                        │
            │ │                                                                                        │
            │ │                                                                                        │
            │ │                                                                                        │
            │ │                                                                                        │
            │ │                                                                                        │
            │ │                                                                                        │
            │ │                                                                                        │
            │ │                                                                                        │
            │ │                                                                                        │
            │ │                                                                                        │
            │ │                                                                                        │
            │ │                                                                                        │
            │ │                                                                                        │
            │ │                                                                                        │
            │ │                                                                                        │
            │ │                                                                                        │
            │ └────────────────────────────────────────────────────────────────────────────────────────┘
────────────────────────────────────────────────────────────────────────────────────────────────────────
nothing playing                    │repeat off · shuffle off        │next
Enter on an album to start         │Enter play · > skip · h back    │end of queue
                                   │                                │
                                   │                                │
```
