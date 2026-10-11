// Measures the checker against gold corrections and against Hunspell on the same dictionary.
//
// usage: measure <dictionary folder> <data folder> [--no-hunspell] [--dump <file>] [--only <corpus,corpus>]
//
// The data folder holds the public misspelling corpora, each file read if present:
//   spell-testset1.txt, spell-testset2.txt   Norvig's, from Mitton's Birkbeck corpus ("right: wrong wrong")
//   missp.dat, holbrook-missp.dat, aspell.dat Mitton's Birkbeck, Holbrook and Aspell sets ("$right" then
//                                             wrongs, one a line, Holbrook's with a count after)
//   wikipedia-for-machines.txt                Wikipedia's "Lists of common misspellings/For machines",
//                                             raw wikitext ("wrong->right, other right")
//   big.txt                                   Norvig's plain text, for check() speed and flags on real text
// Hunspell is run as `hunspell` from PATH (or $HUNSPELL) with `-d <dictionary folder>/index`, so both
// checkers use the same word list. It also writes a one-word dictionary into the data folder to
// measure what Hunspell's process costs without the word list.
use cowrite_spell::Dictionary;
use std::collections::HashSet;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

// how many times a load is repeated, the median kept: 5 smooths the disk cache warming up
const LOAD_RUNS: usize = 5;

// how many suggestions count toward "top 5": the checker's own MAX_SUGGESTIONS
const TOP_LIST: usize = cowrite_spell::MAX_SUGGESTIONS;

// one misspelling and every correction the corpus accepts for it
struct Pair {
    wrong: String,
    right: Vec<String>,
}

// what one checker said about one misspelling: whether it flagged it, and its suggestions
struct Answer {
    flagged: bool,
    suggestions: Vec<String>,
    took: Duration,
}

// the counts for one corpus and one checker
#[derive(Default)]
struct Score {
    pairs: usize,
    not_flagged: usize,
    unreachable: usize,
    evaluated: usize,
    top1: usize,
    top5: usize,
    none: usize,
    shown: usize,
    splits_shown: usize,
    latencies: Vec<Duration>,
}

fn main() {
    let arguments: Vec<String> = std::env::args().collect();
    let usage = "usage: measure <dictionary folder> <data folder> [--no-hunspell] [--dump <file>] [--only <corpus,corpus>]";
    let folder = PathBuf::from(arguments.get(1).expect(usage));
    let data = PathBuf::from(arguments.get(2).expect(usage));
    let with_hunspell = !arguments.iter().any(|argument| argument == "--no-hunspell");
    let dump_path = arguments
        .iter()
        .position(|argument| argument == "--dump")
        .and_then(|at| arguments.get(at + 1));
    // a comma list of corpus names to run, the rest skipped (for quick runs on a subset)
    let only: Option<Vec<&str>> = arguments
        .iter()
        .position(|argument| argument == "--only")
        .and_then(|at| arguments.get(at + 1))
        .map(|list| list.split(',').collect());
    let hunspell_binary = std::env::var("HUNSPELL").unwrap_or_else(|_| "hunspell".to_string());

    // load, timed, and the memory it takes
    // memory is measured around the first load and the first suggestion, before the loads that
    // are only timed: a load's freed working memory stays with the process, so measuring after
    // several would count more than one dictionary
    let aff = std::fs::read_to_string(folder.join("index.aff")).expect("no index.aff");
    let dic = std::fs::read_to_string(folder.join("index.dic")).expect("no index.dic");
    let rss_before = rss_kib(std::process::id());
    let mut load_times = Vec::new();
    let started = Instant::now();
    let dictionary = Dictionary::parse(&aff, &dic).expect("the dictionary could not be read");
    load_times.push(started.elapsed());
    let rss_after = rss_kib(std::process::id());
    // what the first suggestion adds: the suggestion index is built when one is first asked for
    let started = Instant::now();
    dictionary.suggest("teh", &HashSet::new());
    let first_suggest = started.elapsed();
    let rss_after_suggest = rss_kib(std::process::id());
    for _ in 1..LOAD_RUNS {
        let started = Instant::now();
        let again = Dictionary::parse(&aff, &dic).expect("the dictionary could not be read");
        load_times.push(started.elapsed());
        drop(again);
    }
    drop((aff, dic));
    load_times.sort();
    let our_load = load_times[LOAD_RUNS / 2];
    let personal: HashSet<String> = HashSet::new();

    let hunspell_load = with_hunspell.then(|| {
        let mut times: Vec<Duration> = (0..LOAD_RUNS)
            .map(|_| {
                let started = Instant::now();
                let mut hunspell = Hunspell::start(&hunspell_binary, &folder.join("index"));
                hunspell.ask("the");
                started.elapsed()
            })
            .collect();
        times.sort();
        times[LOAD_RUNS / 2]
    });
    // Hunspell's memory: its process with the word list, less its process with a one-word list
    let hunspell_rss = with_hunspell.then(|| {
        let tiny = data.join("tiny");
        std::fs::create_dir_all(&tiny).expect("cannot write the one-word dictionary");
        std::fs::copy(folder.join("index.aff"), tiny.join("index.aff")).unwrap();
        std::fs::write(tiny.join("index.dic"), "1\na\n").unwrap();
        let mut full = Hunspell::start(&hunspell_binary, &folder.join("index"));
        full.ask("the");
        let mut empty = Hunspell::start(&hunspell_binary, &tiny.join("index"));
        empty.ask("the");
        let sizes = (rss_kib(full.child.id()), rss_kib(empty.child.id()));
        std::fs::remove_dir_all(&tiny).ok();
        sizes
    });

    // the corpora
    let corpora: Vec<(&str, Vec<Pair>)> = [
        ("norvig-1", "spell-testset1.txt"),
        ("norvig-2", "spell-testset2.txt"),
        ("birkbeck", "missp.dat"),
        ("holbrook", "holbrook-missp.dat"),
        ("aspell", "aspell.dat"),
        ("wikipedia", "wikipedia-for-machines.txt"),
    ]
    .into_iter()
    .filter(|(name, _)| only.as_ref().is_none_or(|only| only.contains(name)))
    .filter_map(|(name, file)| {
        let text = std::fs::read(data.join(file)).ok()?;
        let text = String::from_utf8_lossy(&text);
        let pairs = match file {
            "spell-testset1.txt" | "spell-testset2.txt" => norvig(&text),
            "wikipedia-for-machines.txt" => wikipedia(&text),
            _ => mitton(&text),
        };
        Some((name, pairs))
    })
    .collect();

    let mut hunspell =
        with_hunspell.then(|| Hunspell::start(&hunspell_binary, &folder.join("index")));
    let mut dump =
        dump_path.map(|path| std::fs::File::create(path).expect("cannot write the dump"));
    let mut results: Vec<(&str, Score, Option<Score>)> = Vec::new();
    for (name, pairs) in &corpora {
        let mut ours = Score::default();
        let mut theirs = Score::default();
        for pair in pairs {
            // a correction the dictionary does not hold cannot be suggested by either checker
            let reachable = pair.right.iter().any(|right| {
                right
                    .split(' ')
                    .all(|part| dictionary.check(part, &personal))
            });
            let started = Instant::now();
            let flagged = !dictionary.check(&pair.wrong, &personal);
            let suggestions = match flagged {
                true => dictionary.suggest(&pair.wrong, &personal),
                false => Vec::new(),
            };
            let our_answer = Answer {
                flagged,
                suggestions,
                took: started.elapsed(),
            };
            tally(&mut ours, pair, &our_answer, reachable);
            let their_answer = hunspell.as_mut().map(|hunspell| hunspell.ask(&pair.wrong));
            if let Some(answer) = &their_answer {
                tally(&mut theirs, pair, answer, reachable);
            }
            if let Some(file) = dump.as_mut() {
                writeln!(
                    file,
                    "{name}\t{}\t{}\t{}\t{}\t{}",
                    pair.wrong,
                    pair.right.join("|"),
                    if reachable {
                        "reachable"
                    } else {
                        "unreachable"
                    },
                    our_answer.suggestions.join(", "),
                    their_answer
                        .map(|answer| answer.suggestions.join(", "))
                        .unwrap_or_default()
                )
                .unwrap();
            }
        }
        results.push((name, ours, with_hunspell.then_some(theirs)));
    }

    // check() speed, and flags on real text, over the words of big.txt
    let text = std::fs::read(data.join("big.txt"))
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
    let text_words: Vec<&str> = text
        .as_deref()
        .map(|text| {
            text.split(|character: char| !(character.is_alphabetic() || character == '\''))
                .map(|word| word.trim_matches('\''))
                .filter(|word| !word.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let started = Instant::now();
    let mut our_flags: HashSet<&str> = HashSet::new();
    for word in &text_words {
        if !dictionary.check(word, &personal) {
            our_flags.insert(word);
        }
    }
    let our_check_time = started.elapsed();
    let their_check = (with_hunspell && !text_words.is_empty())
        .then(|| hunspell_list(&hunspell_binary, &folder.join("index"), &text_words));

    // gold corrections, every one a real word: how many each checker flags
    let gold: HashSet<&str> = corpora
        .iter()
        .flat_map(|(_, pairs)| pairs.iter().flat_map(|pair| pair.right.iter()))
        .filter(|right| !right.contains(' '))
        .map(String::as_str)
        .collect();
    let gold_ours = gold
        .iter()
        .filter(|word| !dictionary.check(word, &personal))
        .count();
    let gold_theirs = hunspell.as_mut().map(|hunspell| {
        gold.iter()
            .filter(|word| hunspell.ask(word).flagged)
            .count()
    });

    // the tables
    println!("## Suggestion quality against gold corrections\n");
    println!(
        "Accuracy is over the evaluated misspellings: flagged by the checker and with a correction the dictionary holds.\n"
    );
    println!("| corpus | metric | ours | hunspell |");
    println!("|---|---|---|---|");
    for (name, ours, theirs) in &results {
        let rows: [(&str, fn(&Score) -> String); 9] = [
            ("pairs (misspelling, correction)", |s| s.pairs.to_string()),
            ("misspelling accepted as a word (not flagged)", |s| {
                s.not_flagged.to_string()
            }),
            ("correction not in dictionary (unreachable)", |s| {
                s.unreachable.to_string()
            }),
            ("evaluated", |s| s.evaluated.to_string()),
            ("top-1 accuracy", |s| percent(s.top1, s.evaluated)),
            ("top-5 accuracy", |s| percent(s.top5, s.evaluated)),
            ("no suggestion", |s| percent(s.none, s.evaluated)),
            ("split suggestions among those shown (\"co lour\")", |s| {
                percent(s.splits_shown, s.shown)
            }),
            ("suggest latency p50 / p95 / max", |s| latency(&s.latencies)),
        ];
        for (label, value) in rows {
            let their_value = theirs.as_ref().map(value).unwrap_or_else(|| "-".into());
            println!("| {name} | {label} | {} | {their_value} |", value(ours));
        }
    }
    let total = |pick: fn(&Score) -> usize, theirs: bool| -> usize {
        results
            .iter()
            .filter_map(|(_, ours, their)| if theirs { their.as_ref() } else { Some(ours) })
            .map(pick)
            .sum()
    };
    for theirs in [false, true] {
        if theirs && !with_hunspell {
            continue;
        }
        let evaluated = total(|s| s.evaluated, theirs);
        println!(
            "| all (sum) | top-1 / top-5 / none, {} | {} / {} / {} | |",
            if theirs { "hunspell" } else { "ours" },
            percent(total(|s| s.top1, theirs), evaluated),
            percent(total(|s| s.top5, theirs), evaluated),
            percent(total(|s| s.none, theirs), evaluated)
        );
    }

    println!("\n## Flags on correct words\n");
    println!("| list | metric | ours | hunspell |");
    println!("|---|---|---|---|");
    println!(
        "| gold corrections, distinct ({}) | flagged as misspelled | {gold_ours} | {} |",
        gold.len(),
        gold_theirs.map_or("-".into(), |count| count.to_string())
    );
    // Hunspell's own tokenizer splits a word at an apostrophe ("couldn't" is judged as "couldn"),
    // so the flags are compared on the words without one
    let distinct: HashSet<&str> = text_words
        .iter()
        .copied()
        .filter(|word| !word.contains('\''))
        .collect();
    let lower_distinct: HashSet<&str> = distinct
        .iter()
        .copied()
        .filter(|word| word.chars().next().is_some_and(char::is_lowercase))
        .collect();
    let distinct_list: Vec<&str> = distinct.iter().copied().collect();
    let their_distinct = (with_hunspell && !distinct_list.is_empty())
        .then(|| hunspell_list(&hunspell_binary, &folder.join("index"), &distinct_list).0);
    let their_flags = their_distinct.as_ref();
    for (label, words) in [
        ("big.txt distinct words without an apostrophe", &distinct),
        (
            "big.txt distinct lower-case-initial words without an apostrophe",
            &lower_distinct,
        ),
    ] {
        let ours_count = words
            .iter()
            .filter(|word| our_flags.contains(*word))
            .count();
        let theirs_count =
            their_flags.map(|flags| words.iter().filter(|word| flags.contains(**word)).count());
        println!(
            "| {label} ({}) | flagged | {ours_count} | {} |",
            words.len(),
            theirs_count.map_or("-".into(), |count| count.to_string())
        );
        if let Some(flags) = their_flags {
            let only_ours = words
                .iter()
                .filter(|w| our_flags.contains(**w) && !flags.contains(**w))
                .count();
            let only_theirs = words
                .iter()
                .filter(|w| !our_flags.contains(**w) && flags.contains(**w))
                .count();
            println!("| {label} | flagged by one checker only | {only_ours} | {only_theirs} |");
        }
    }

    println!("\n## Speed and size\n");
    println!("| metric | ours | hunspell |");
    println!("|---|---|---|");
    println!(
        "| dictionary load (median of {LOAD_RUNS}; Hunspell's includes starting its process) | {} | {} |",
        millis(our_load),
        hunspell_load.map_or("-".into(), millis)
    );
    let words_per_second =
        |count: usize, took: Duration| format!("{:.0} words/s", count as f64 / took.as_secs_f64());
    println!(
        "| check() over big.txt ({} words; Hunspell's is `-l` less its load) | {} | {} |",
        text_words.len(),
        words_per_second(text_words.len(), our_check_time),
        their_check
            .as_ref()
            .zip(hunspell_load)
            .map_or("-".into(), |((_, took), load)| words_per_second(
                text_words.len(),
                took.saturating_sub(load)
            ))
    );
    let all_ours: Vec<Duration> = results
        .iter()
        .flat_map(|(_, ours, _)| ours.latencies.clone())
        .collect();
    let all_theirs: Vec<Duration> = results
        .iter()
        .filter_map(|(_, _, theirs)| theirs.as_ref())
        .flat_map(|theirs| theirs.latencies.clone())
        .collect();
    println!(
        "| suggest latency p50 / p95 / max, every flagged misspelling (Hunspell's through its pipe) | {} | {} |",
        latency(&all_ours),
        if all_theirs.is_empty() {
            "-".into()
        } else {
            latency(&all_theirs)
        }
    );
    println!(
        "| memory after load (RSS delta; Hunspell's is its process less its process with a one-word list) | {:.1} MiB | {} |",
        rss_after.saturating_sub(rss_before) as f64 / 1024.0,
        hunspell_rss.map_or("-".into(), |(full, empty)| format!(
            "{:.1} MiB ({:.1} MiB whole process)",
            full.saturating_sub(empty) as f64 / 1024.0,
            full as f64 / 1024.0
        ))
    );
    println!(
        "| first suggestion, which builds the suggestion index | {} | - |",
        millis(first_suggest)
    );
    println!(
        "| memory once suggestions are built (RSS delta) | {:.1} MiB | - |",
        rss_after_suggest.saturating_sub(rss_before) as f64 / 1024.0
    );
    let check_binary = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("check")))
        .and_then(|path| std::fs::metadata(path).ok())
        .map_or("- (build the check example)".into(), |meta| {
            format!("{:.0} KiB", meta.len() as f64 / 1024.0)
        });
    println!("| check example binary (this build's profile) | {check_binary} | - |");
    let library = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs"))
        .unwrap_or_default();
    println!(
        "| src/lib.rs nonblank lines | {} | - |",
        library
            .lines()
            .filter(|line| !line.trim().is_empty())
            .count()
    );
}

// count one checker's answer to one misspelling
fn tally(score: &mut Score, pair: &Pair, answer: &Answer, reachable: bool) {
    score.pairs += 1;
    if !answer.flagged {
        score.not_flagged += 1;
        return;
    }
    score.latencies.push(answer.took);
    // a split is the misspelling cut in two by a space or hyphen ("co lour" for "colour")
    let joined = |suggestion: &String| suggestion.replace([' ', '-'], "").to_lowercase();
    for suggestion in answer.suggestions.iter().take(TOP_LIST) {
        score.shown += 1;
        if suggestion.contains([' ', '-']) && joined(suggestion) == pair.wrong.to_lowercase() {
            score.splits_shown += 1;
        }
    }
    if !reachable {
        score.unreachable += 1;
        return;
    }
    score.evaluated += 1;
    let is_right = |suggestion: &String| pair.right.contains(suggestion);
    if answer.suggestions.is_empty() {
        score.none += 1;
    }
    if answer.suggestions.first().is_some_and(is_right) {
        score.top1 += 1;
    }
    if answer.suggestions.iter().take(TOP_LIST).any(is_right) {
        score.top5 += 1;
    }
}

// Norvig's "right: wrong wrong" lines
fn norvig(text: &str) -> Vec<Pair> {
    let mut pairs = Vec::new();
    for line in text.lines() {
        let Some((right, wrongs)) = line.split_once(':') else {
            continue;
        };
        for wrong in wrongs.split_whitespace() {
            pairs.push((wrong.to_string(), vec![right.trim().to_string()]));
        }
    }
    distinct(pairs)
}

// Mitton's "$right" lines each followed by its misspellings; `_` is a space, `$?` an unknown word
fn mitton(text: &str) -> Vec<Pair> {
    let mut pairs = Vec::new();
    let mut right: Option<String> = None;
    for line in text.lines() {
        if let Some(word) = line.strip_prefix('$') {
            right = (word != "?").then(|| word.replace('_', " "));
            continue;
        }
        let Some(right) = &right else {
            continue;
        };
        // Holbrook's lines carry a count after the word
        let Some(wrong) = line.split_whitespace().next() else {
            continue;
        };
        if !wrong.contains('_') {
            pairs.push((wrong.to_string(), vec![right.clone()]));
        }
    }
    distinct(pairs)
}

// Wikipedia's " wrong->right, other right" lines
fn wikipedia(text: &str) -> Vec<Pair> {
    let pairs = text
        .lines()
        .filter_map(|line| line.trim().split_once("->"))
        .filter(|(wrong, _)| !wrong.contains(' '))
        .map(|(wrong, rights)| {
            let rights = rights
                .split(',')
                .map(|right| right.trim().to_string())
                .filter(|right| !right.is_empty());
            (wrong.to_string(), rights.collect())
        })
        .collect();
    distinct(pairs)
}

// each (misspelling, corrections) once; a misspelling that equals its correction is no misspelling
fn distinct(pairs: Vec<(String, Vec<String>)>) -> Vec<Pair> {
    let mut seen = HashSet::new();
    pairs
        .into_iter()
        .filter(|(wrong, right)| {
            !right.contains(wrong) && seen.insert((wrong.clone(), right.clone()))
        })
        .map(|(wrong, right)| Pair { wrong, right })
        .collect()
}

// Hunspell in pipe mode (`-a`), asked one word at a time
struct Hunspell {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}

impl Hunspell {
    fn start(binary: &str, dictionary: &Path) -> Hunspell {
        let mut child = Command::new(binary)
            .arg("-d")
            .arg(dictionary)
            .arg("-a")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("cannot run hunspell");
        let input = child.stdin.take().unwrap();
        let mut output = BufReader::new(child.stdout.take().unwrap());
        // the version banner
        let mut banner = String::new();
        output.read_line(&mut banner).unwrap();
        Hunspell {
            child,
            input,
            output,
        }
    }

    // a word's verdict: its answer lines run to a blank line, one per piece Hunspell split it
    // into; `&` carries suggestions, `#` is wrong with none, anything else is right. `^` keeps a
    // word that starts with a command character from being read as a command
    fn ask(&mut self, word: &str) -> Answer {
        let started = Instant::now();
        writeln!(self.input, "^{word}").unwrap();
        self.input.flush().unwrap();
        let mut flagged = false;
        let mut suggestions: Option<Vec<String>> = None;
        loop {
            let mut line = String::new();
            if self.output.read_line(&mut line).unwrap() == 0 {
                break;
            }
            let line = line.trim_end_matches('\n');
            if line.is_empty() {
                break;
            }
            match line.chars().next() {
                Some('&') => {
                    flagged = true;
                    if suggestions.is_none() {
                        let list = line.split_once(": ").map_or("", |(_, list)| list);
                        suggestions = Some(list.split(", ").map(str::to_string).collect());
                    }
                }
                Some('#') => flagged = true,
                _ => {}
            }
        }
        Answer {
            flagged,
            suggestions: suggestions.unwrap_or_default(),
            took: started.elapsed(),
        }
    }
}

impl Drop for Hunspell {
    fn drop(&mut self) {
        self.child.kill().ok();
        self.child.wait().ok();
    }
}

// the words `hunspell -l` flags among a list, and how long the whole run took
fn hunspell_list(binary: &str, dictionary: &Path, words: &[&str]) -> (HashSet<String>, Duration) {
    let started = Instant::now();
    let mut child = Command::new(binary)
        .arg("-d")
        .arg(dictionary)
        .arg("-l")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("cannot run hunspell");
    let mut input = child.stdin.take().unwrap();
    let text: String = words.iter().map(|word| format!("{word}\n")).collect();
    // written from another thread so neither pipe fills while the other waits
    let writer = std::thread::spawn(move || {
        input.write_all(text.as_bytes()).unwrap();
    });
    let output = child.wait_with_output().unwrap();
    writer.join().unwrap();
    let flags = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    (flags, started.elapsed())
}

// a process's resident memory in KiB, from /proc
fn rss_kib(process: u32) -> u64 {
    let status = std::fs::read_to_string(format!("/proc/{process}/status")).unwrap_or_default();
    status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .and_then(|value| value.trim().trim_end_matches("kB").trim().parse().ok())
        .unwrap_or(0)
}

fn percent(part: usize, whole: usize) -> String {
    match whole {
        0 => "-".into(),
        _ => format!("{:.1}%", 100.0 * part as f64 / whole as f64),
    }
}

fn millis(duration: Duration) -> String {
    format!("{:.1} ms", duration.as_secs_f64() * 1000.0)
}

// p50, p95 and max of a set of timings
fn latency(times: &[Duration]) -> String {
    if times.is_empty() {
        return "-".into();
    }
    let mut sorted = times.to_vec();
    sorted.sort();
    let at = |fraction: f64| sorted[((sorted.len() - 1) as f64 * fraction) as usize];
    format!(
        "{} / {} / {}",
        millis(at(0.5)),
        millis(at(0.95)),
        millis(sorted[sorted.len() - 1])
    )
}
