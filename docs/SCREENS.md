# Screens

Rendered by `scripts/screens.py` from the real binary driving a pty
against `scripts/fixture_server.py`, so what is written here is what the
interface draws. Regenerate with
`python3 scripts/screens.py ./target/release/undertone`.

Covers are shown as their placeholder slot: the generator runs with
`--no-art` so these frames are identical on every machine and CI can
diff them. What the art itself looks like in each protocol is in
`docs/COMPATIBILITY.md`, and is asserted there rather than drawn here.

## The library

Album list, with the rail, the list's own header and the three-column bar.

```
▸ Albums   1│ Albums · random                                                                      1 / 3
  Artists  2│ ▸ ▚ ▚  Fixture One
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
            │
            │
────────────────────────────────────────────────────────────────────────────────────────────────────────
nothing playing                    │fixture 1.16.1                  │next
Enter on an album to start         │S sort · w layout · Enter · *   │end of queue
                                   │star · ? help                   │
                                   │                                │
```

## The wall

`w` turns any album list into a wall of covers; each list keeps its own choice.

```
▸ Albums   1│ Albums · random                                                                      1 / 3
  Artists  2│   ▚ ▚ ▚ ▚ ▚      ▚ ▚ ▚ ▚ ▚      ▚ ▚ ▚ ▚ ▚
  Starred  3│
  Later    4│
  Lists    5│
  Queue    6│
            │ ▸Fixture One    Fixture Two    Third
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
nothing playing                    │albums: wall                    │next
Enter on an album to start         │S sort · w layout · Enter · *   │end of queue
                                   │star · ? help                   │
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
            │   1. Track 1  3:20
            │   2. Track 2  3:20
            │   3. Track 3  3:20
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
nothing playing                    │albums: list                    │next
Enter on an album to start         │Enter play · * star · A artist ·│end of queue
                                   │h back                          │
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
Enter on an album to start         │S sort · w layout · Enter open ·│end of queue
                                   │h back                          │
                                   │                                │
```

## An artist

Three panes: albums, similar artists, biography. Tab moves between them.

```
  Albums   1│ Artist                                                                         Test Artist
▸ Artists  2│ albums · top songs                            │ similar
  Starred  3│ ▸ ▚ ▚  Fixture One                            │ Similar One
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
            │                                               │
            │                                               │
────────────────────────────────────────────────────────────────────────────────────────────────────────
nothing playing                    │repeat off · shuffle off        │next
Enter on an album to start         │Tab pane · t songs · Enter open │end of queue
                                   │· h back                        │
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
            │                                               │
            │                                               │
────────────────────────────────────────────────────────────────────────────────────────────────────────
nothing playing                    │artist: top songs               │next
Enter on an album to start         │Tab pane · t songs · Enter open │end of queue
                                   │· h back                        │
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
nothing playing                    │artist: top songs               │next
Enter on an album to start         │Enter play · > skip · h back    │end of queue
                                   │                                │
                                   │                                │
```
