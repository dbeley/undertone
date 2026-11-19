# Screens

Rendered by `scripts/screens.py` from the real binary driving a pty
against `scripts/fixture_server.py`, so what is written here is what the
interface draws. Regenerate with
`python3 scripts/screens.py ./target/release/undertone`.

Covers are shown as their placeholder slot: the fixture server serves no
cover art, so these frames are identical on every machine and CI can
diff them. The app is started as a plain `xterm-256color`, which pins the
art protocol too: a pixel terminal gives a cover a slightly different
number of cells, and these frames are about layout, not about pixels.
What the art itself looks like in each protocol is in
`docs/COMPATIBILITY.md`, and is asserted there rather than drawn here.

## The library

What the app opens on: a wall of covers, with the rail and the three-column bar.

```
▸ Albums   1│ Albums · random                                                                      1 / 3
  Artists  2│   ▚ ▚ ▚ ▚ ▚      ▚ ▚ ▚ ▚ ▚      ▚ ▚ ▚ ▚ ▚
  Starred  3│
  Later    4│
  Lists    5│
  Queue    6│
            │ Fixture One    Fixture Two    Third
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
  Search   /│
  Help     ?│
────────────────────────────────────────────────────────────────────────────────────────────────────────
nothing playing                    │fixture 1.16.1                  │next
Enter on an album to start         │S sort · w layout · a next · A  │end of queue
                                   │end · * star                    │
                                   │                                │
```

## The list

`w` turns any wall back into rows; each list keeps its own choice.

```
▸ Albums   1│ Albums · random                                                                      1 / 3
  Artists  2│ ▚ ▚ ▚  Fixture One
  Starred  3│        Test Artist · 2000
  Later    4│        3 tracks
  Lists    5│ ▚ ▚ ▚  Fixture Two
  Queue    6│        Other Band · 2001
            │        3 tracks
            │ ▚ ▚ ▚  Third
            │        Nobody · 2002
            │        3 tracks
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
  Search   /│
  Help     ?│
────────────────────────────────────────────────────────────────────────────────────────────────────────
nothing playing                    │albums: list                    │next
Enter on an album to start         │S sort · w layout · a next · A  │end of queue
                                   │end · * star                    │
                                   │                                │
```

## An album

The album's cover and its tracks; the bar keeps playing state.

```
▸ Albums   1│ Album                                                                          Fixture One
  Artists  2│                    Fixture One
  Starred  3│                    Test Artist
  Later    4│                    2000 · 3 tracks
  Lists    5│
  Queue    6│
            │
            │
            │
            │
            │   1. Track 1  Test Artist  3:20
            │   2. Track 2  Test Artist  3:20
            │   3. Track 3  Test Artist  3:20
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
  Search   /│
  Help     ?│
────────────────────────────────────────────────────────────────────────────────────────────────────────
nothing playing                    │albums: wall                    │next
Enter on an album to start         │Enter play · a next · A end · i │end of queue
                                   │artist · * star · h back        │
                                   │                                │
```

## Search

One flat result list: albums first, then songs, each row labelled.

```
  Albums   1│ Search · fixture                                                                     1 / 6
  Artists  2│ album  Fixture One  Test Artist
  Starred  3│ album  Fixture Two  Other Band
  Later    4│ album  Third  Nobody
  Lists    5│ song   Track 1  Test Artist
  Queue    6│ song   Track 2  Test Artist
            │ song   Track 3  Test Artist
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
▸ Search   /│
  Help     ?│
────────────────────────────────────────────────────────────────────────────────────────────────────────
nothing playing                    │6 results for “fixture”         │next
Enter on an album to start         │/ edit · Enter open/play · o/i  │end of queue
                                   │album/artist · a/A queue        │
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
  Search   /│
  Help     ?│
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
▸ Artists  2│
  Starred  3│
  Later    4│
  Lists    5│
  Queue    6│
            │ Test Artist
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
  Search   /│
  Help     ?│
────────────────────────────────────────────────────────────────────────────────────────────────────────
nothing playing                    │S sort · w layout · Enter open ·│next
Enter on an album to start         │* star · h back                 │end of queue
                                   │                                │
                                   │                                │
```

## An artist

Three panes: albums, similar artists, biography. Tab moves between them.

```
  Albums   1│ Artist                                                                         Test Artist
▸ Artists  2│ albums · top songs                            │ similar
  Starred  3│ ▚ ▚ ▚  Fixture One                            │ Similar One
  Later    4│        Test Artist · 2000                     │ Similar Two
  Lists    5│        3 tracks                               │
  Queue    6│ ▚ ▚ ▚  Fixture Two                            │
            │        Other Band · 2001                      │
            │        3 tracks                               │
            │ ▚ ▚ ▚  Third                                  │
            │        Nobody · 2002                          │
            │        3 tracks                               │
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
  Search   /│                                               │
  Help     ?│                                               │
────────────────────────────────────────────────────────────────────────────────────────────────────────
nothing playing                    │Tab pane · t songs · a next · A │next
Enter on an album to start         │end · * star · h back           │end of queue
                                   │                                │
                                   │                                │
```

## An artist, top songs

The left pane has two tabs: albums and the artist's most played tracks.

```
  Albums   1│ Artist                                                                         Test Artist
▸ Artists  2│ albums · top songs                            │ similar
  Starred  3│   1. Top Track 1  Fixture One  3:00           │ Similar One
  Later    4│   2. Top Track 2  Fixture One  3:01           │ Similar Two
  Lists    5│   3. Top Track 3  Fixture One  3:02           │
  Queue    6│   4. Top Track 4  Fixture One  3:03           │
            │                                               │
            │                                               │
            │                                               │
            │                                               │
            │                                               │
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
  Search   /│                                               │
  Help     ?│                                               │
────────────────────────────────────────────────────────────────────────────────────────────────────────
nothing playing                    │artist: top songs               │next
Enter on an album to start         │Tab pane · t songs · a next · A │end of queue
                                   │end · * star · h back           │
                                   │                                │
```

## The queue

What plays next; Enter starts from the cursor.

```
  Albums   1│ Queue                                                                                    0
  Artists  2│ ┌ Queue: 0 tracks · shuffle off · repeat off ────────────────────────────────────────────┐
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
  Search   /│ │                                                                                        │
  Help     ?│ └────────────────────────────────────────────────────────────────────────────────────────┘
────────────────────────────────────────────────────────────────────────────────────────────────────────
nothing playing                    │artist: top songs               │next
Enter on an album to start         │Enter play · x delete · C clear │end of queue
                                   │· J/K move · o/i album/artist   │
                                   │                                │
```
