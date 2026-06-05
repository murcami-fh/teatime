# 🍵 Tea Time

Tea Time (`tt`) is a [fend](https://printfn.github.io/fend/documentation/)-inspired command-line utility oriented towards time calculations.

**This is a vibe-coded application.**

## Usage

```
$ tt
> 1d 1h + 4h 3m
1d 5h 3m
> now
13h 15m 2s
> now to m
approx. 795.033333333 m
> now to h m
approx. 13 h 15 m
> 1.2h % 15m
12 m
> 1h / 3 to 2dp
approx. 0.33h
> func a b := a + b
> func 1d 3h
1d 3h
> curry := func 1d
> curry 3h
1d 3h
> _ + 1h
1d 4h
```

## Configuration

Configuration is defined in `~/.local/tt/config.toml` (or generally in
`${LOCAL}/tt/config.toml`, where `${LOCAL}` is as specified by [the
directories crate](https://docs.rs/directories/latest/directories/)).

```TOML
/* Define custom units */
[units]
[units.manday]
name = "manday"
aliases = ["md", "devday", "dd", "mandays"]
value = "8h"

/* Define functional expressions */
[functions]
[functions.example]
name = "example"
aliases = ["ex", "e"]
arguments = ["arg1", "arg2"]
definition = "1d 3m + arg1 - arg2"
```

## Installation

```
$ cargo build --release
...
    Finished
$ ln -s ~/.local/bin/tt `realpath target/release/tt`
```
