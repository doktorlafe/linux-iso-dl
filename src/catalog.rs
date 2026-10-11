//! The built-in list of images, read from `catalog.txt`.

pub struct Image {
    /// Short name used on the command line.
    pub id: &'static str,
    pub distro: &'static str,
    pub edition: &'static str,
    /// Where to find the current file, see `net::resolve`.
    pub source: &'static str,
}

impl Image {
    /// True when every word of the filter occurs somewhere in the entry.
    pub fn matches(&self, filter: &str) -> bool {
        let text = format!("{} {} {}", self.id, self.distro, self.edition).to_lowercase();
        filter
            .to_lowercase()
            .split_whitespace()
            .all(|word| text.contains(word))
    }
}

pub fn all() -> Vec<Image> {
    let mut images = Vec::new();
    for (i, line) in include_str!("catalog.txt").lines().enumerate() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split('|').map(str::trim).collect();
        let [id, distro, edition, source] = fields[..] else {
            panic!("catalog.txt:{}: čekám 4 pole oddělená '|'", i + 1);
        };
        images.push(Image {
            id,
            distro,
            edition,
            source,
        });
    }
    images
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_is_well_formed() {
        let images = all();
        assert!(!images.is_empty());
        for (i, image) in images.iter().enumerate() {
            assert!(
                !image.id.is_empty() && !image.id.contains(char::is_whitespace),
                "{:?}",
                image.id
            );
            assert!(
                image.source.starts_with("http") || image.source.starts_with("scan http"),
                "{}",
                image.id
            );
            assert!(
                images[..i].iter().all(|other| other.id != image.id),
                "{} je v katalogu dvakrát",
                image.id
            );
        }
    }

    #[test]
    fn filter_needs_every_word() {
        let images = all();
        let debian = images.iter().find(|i| i.id == "debian-netinst").unwrap();
        assert!(debian.matches(""));
        assert!(debian.matches("DEB net"));
        assert!(!debian.matches("debian kde"));
    }
}
