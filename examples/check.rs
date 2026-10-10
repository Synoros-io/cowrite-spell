// Reads a dictionary folder (index.aff and index.dic) given as the first argument, then words
// from stdin, and prints each misspelled word with what it was probably meant to be.
use cowrite_spell::Dictionary;
use std::collections::HashSet;
use std::io::Read;

fn main() {
    let folder = std::env::args()
        .nth(1)
        .expect("usage: check <dictionary folder> < words");
    let aff = std::fs::read_to_string(format!("{folder}/index.aff")).expect("no index.aff");
    let dic = std::fs::read_to_string(format!("{folder}/index.dic")).expect("no index.dic");
    let dictionary = Dictionary::parse(&aff, &dic).expect("the dictionary could not be read");

    // no added words here; a real caller passes the person's own
    let personal = HashSet::new();
    let mut text = String::new();
    std::io::stdin()
        .read_to_string(&mut text)
        .expect("stdin was not UTF-8");
    for word in text.split_whitespace() {
        if !dictionary.check(word, &personal) {
            println!("{word}: {}", dictionary.suggest(word, &personal).join(", "));
        }
    }
}
