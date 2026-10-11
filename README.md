# linux-iso-dl

Downloads installation images of Linux distributions: check them in a list,
press `d`, and they land in one folder.

- the list is built in and covers about 130 images of some 50 distributions
- every image is looked up on the distribution's server at download time, so
  the list keeps pointing at the current release without being updated
- an interrupted download continues where it stopped
- usable through an interactive terminal UI or through commands

The interface and its messages are in Czech.

## Installation

Requires [Rust](https://rustup.rs) and a C compiler (`gcc`).

```sh
sh install.sh
```

The script builds a release binary (in `~/.cache/linux-iso-dl-target`) and
installs it to `~/.local/bin/linux-iso-dl`. Updates are installed the same way.

## Usage

Run it without arguments to open the interactive interface:

```sh
linux-iso-dl
```

| Key | Action |
| --- | --- |
| `↑` `↓` / `j` `k`, `PgUp` `PgDn`, `g` `G` | move |
| `Enter`, `→` `←` | open or close a distribution with several images |
| `Space` | check or uncheck the image; on a distribution, all its images |
| `a` | check everything the filter shows, or uncheck it |
| `/` | filter the list; every word typed must occur in the row |
| `o` | change the folder images are saved to |
| `d` | download the checked images, one after another |
| `x` | cancel the download |
| `q` | quit |

Images are saved to `~/Downloads/linux-iso-dl` unless another folder is set.
While a file is downloading it is called `<name>.part`; a cancelled or broken
download leaves that file behind and the next attempt continues from it. An
image that is already in the folder is not downloaded again.

Settings are stored in `~/.config/linux-iso-dl/config` (set the
`LINUX_ISO_DL_CONFIG` variable to use a different path).

## Commands

Everything in the interface can also be done from the command line. Run
`linux-iso-dl --help` for the list:

```
linux-iso-dl list [text]     list the images, or those containing the text
linux-iso-dl get <id>...     download images to the configured folder
linux-iso-dl dir <folder>    set the folder
linux-iso-dl show            print the current settings
linux-iso-dl check [id]...   verify that the images in the catalog can be found
```

## The catalog

The list lives in `src/catalog.txt`, one image per line:

```
id | distribution | edition | source
```

The source is a URL whose path segments may be patterns. Each pattern is
resolved from the server's directory listing, and the newest match wins:

| Pattern | Matches |
| --- | --- |
| `*` | anything |
| `#` | a run of digits |
| `%` | a version: digits and dots |
| `[abc]` | one of the characters |

```
https://dl.fedoraproject.org/pub/fedora/linux/releases/#/Server/x86_64/iso/Fedora-Server-dvd-*.iso
```

For servers without directory listings the source is `scan <page> <pattern>`,
which takes the newest link on the page whose file name matches. The page can
be a download page or a JSON API such as GitHub's `releases/latest`.

After editing the catalog, run `cargo run -- check` to try every entry
against the real servers; it prints the resolved URL and size of each image.

Checksums and signatures are not verified; the downloads rely on HTTPS.
