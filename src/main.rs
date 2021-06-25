use regex::Regex;
use std::{
    cmp::max,
    error::Error,
    fs::File,
    io::{self, BufRead, BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
    process::exit,
    str::FromStr,
};
use structopt::StructOpt;
use thiserror::Error;

/// Errors for parsing / validating [`FieldRange`] strings.
#[derive(Error, Debug)]
enum FieldError {
    #[error("Fields and positions are numbered from 1: {0}")]
    InvalidField(usize),
    #[error("High end of range less than low end of range: {0}-{1}")]
    InvalidOrder(usize, usize),
    #[error("Failed to parse field: {0}")]
    FailedParse(String),
    #[error("No headers matched")]
    NoHeadersMatched,
}

/// Represent a range of columns to keep.
#[derive(PartialEq, Eq, PartialOrd, Ord, Debug)]
struct FieldRange {
    low: usize,
    high: usize,
    // The initial position of this range in the user input
    pos: usize,
}

impl FromStr for FieldRange {
    type Err = FieldError;

    /// Convert a [`str`] into a [`FieldRange`]
    fn from_str(s: &str) -> Result<FieldRange, FieldError> {
        const MAX: usize = usize::MAX;

        let mut parts = s.splitn(2, '-');

        match (parts.next(), parts.next()) {
            (Some(nm), None) => {
                if let Ok(nm) = nm.parse::<usize>() {
                    if nm > 0 {
                        Ok(FieldRange {
                            low: nm - 1,
                            high: nm - 1,
                            pos: 0,
                        })
                    } else {
                        Err(FieldError::InvalidField(nm))
                    }
                } else {
                    Err(FieldError::FailedParse(nm.to_owned()))
                }
            }
            (Some(n), Some(m)) if m.is_empty() => {
                if let Ok(low) = n.parse::<usize>() {
                    if low > 0 {
                        Ok(FieldRange {
                            low: low - 1,
                            high: MAX - 1,
                            pos: 0,
                        })
                    } else {
                        Err(FieldError::InvalidField(low))
                    }
                } else {
                    Err(FieldError::FailedParse(n.to_owned()))
                }
            }
            (Some(n), Some(m)) if n.is_empty() => {
                if let Ok(high) = m.parse::<usize>() {
                    if high > 0 {
                        Ok(FieldRange {
                            low: 0,
                            high: high - 1,
                            pos: 0,
                        })
                    } else {
                        Err(FieldError::InvalidField(high))
                    }
                } else {
                    Err(FieldError::FailedParse(m.to_owned()))
                }
            }
            (Some(n), Some(m)) => match (n.parse::<usize>(), m.parse::<usize>()) {
                (Ok(low), Ok(high)) => {
                    if low > 0 && low <= high {
                        Ok(FieldRange {
                            low: low - 1,
                            high: high - 1,
                            pos: 0,
                        })
                    } else if low == 0 {
                        Err(FieldError::InvalidField(low))
                    } else {
                        Err(FieldError::InvalidOrder(low, high))
                    }
                }
                _ => Err(FieldError::FailedParse(format!("{}-{}", n, m))),
            },
            _ => unreachable!(),
        }
    }
}

impl FieldRange {
    /// Parse a comma separated list of fields and merge any overlaps
    pub fn from_list(list: &str) -> Result<Vec<FieldRange>, FieldError> {
        let mut ranges: Vec<FieldRange> = vec![];
        for (i, item) in list.split(',').enumerate() {
            let mut rnge: FieldRange = FromStr::from_str(item)?;
            rnge.pos = i;
            ranges.push(rnge);
        }
        FieldRange::post_process_ranges(&mut ranges);

        Ok(ranges)
    }

    /// Get the indices of the headers that match any of the provided regex's.
    pub fn from_header_list(
        list: &[Regex],
        header: &str,
        delim: &Regex,
        literal: bool,
    ) -> Result<Vec<FieldRange>, FieldError> {
        let mut ranges = vec![];
        for (i, header) in delim.split(header).enumerate() {
            for (j, regex) in list.iter().enumerate() {
                if literal {
                    if regex.as_str() == header {
                        ranges.push(FieldRange {
                            low: i,
                            high: i,
                            pos: j,
                        });
                    }
                } else if regex.is_match(header) {
                    ranges.push(FieldRange {
                        low: i,
                        high: i,
                        pos: j,
                    });
                }
            }
        }

        if ranges.is_empty() {
            return Err(FieldError::NoHeadersMatched);
        }

        FieldRange::post_process_ranges(&mut ranges);

        Ok(ranges)
    }

    /// Sort and merge overlaps in a set of [`Vec<FieldRange>`].
    fn post_process_ranges(ranges: &mut Vec<FieldRange>) {
        ranges.sort();
        // merge overlapping ranges
        for i in 0..ranges.len() {
            let j = i + 1;

            while j < ranges.len()
                && ranges[j].low <= ranges[i].high + 1
                && (ranges[j].pos == ranges[i].pos || ranges[j].pos - 1 == ranges[i].pos)
            {
                let j_high = ranges.remove(j).high;
                ranges[i].high = max(ranges[i].high, j_high);
            }
        }
    }
}

/// Determine whether we should read from a file or stdin.
fn select_input<P: AsRef<Path>>(input: Option<P>) -> Result<Box<dyn Read>, io::Error> {
    let reader: Box<dyn Read> = match input {
        Some(path) => {
            if path.as_ref().as_os_str() == "-" {
                Box::new(io::stdin())
            } else {
                Box::new(File::open(path)?)
            }
        }
        None => Box::new(io::stdin()),
    };
    Ok(reader)
}

/// Determine if we should write to a file or stdout.
fn select_output<P: AsRef<Path>>(output: Option<P>) -> Result<Box<dyn Write>, io::Error> {
    let writer: Box<dyn Write> = match output {
        Some(path) => {
            if path.as_ref().as_os_str() == "-" {
                // TODO: verify that stdout buffers when writing to a terminal now (this was a bug in Rust at some point).
                Box::new(io::stdout())
            } else {
                Box::new(File::create(path)?)
            }
        }
        None => Box::new(io::stdout()),
    };
    Ok(writer)
}

/// Select and reorder columns.
///
/// This tool behaves like unix `cut` with a few exceptions:
///
/// * `delimiter` is a regex and not a fixed string
/// * `header-fields` allows for specifying a regex to match header names to select columns
/// * both `header-fields` and `fields` order dictate the order of the output columns
///
/// *Note* regarding output ordering: values are written only once. So for a `fields` value of `4-,1,5-8`,
/// which translates to "print columns 4 through the end and then the last column and then columns 5 through 8",
/// columns 5-8 won't be printed again because they were already consumed by the `4-` range.
///
/// If `field-headers` is used as a regex then the headers will be be grouped together in groups that all matched the
/// same regex, and in the order of the regex as specified on the CLI.
///
/// *Note* if a line does not contain all columns, those columns will not be printed. i.e. jagged rows will remain jagged.
#[derive(Debug, StructOpt)]
#[structopt(name = "hck", about = "A regex based version of cut.")]
struct Opts {
    /// Input files to parse, defaults to stdin
    #[structopt(short, long)]
    input: Option<PathBuf>,

    /// Output file to write to, defaults to stdout
    #[structopt(short, long)]
    output: Option<PathBuf>,

    /// Regex delimiter to use on input files
    #[structopt(short, long, default_value = r"\t")]
    delimiter: Regex,

    /// Delimiter string to use on outputs
    #[structopt(short = "D", long, default_value = "\t")]
    output_delimiter: String,

    /// Fields to keep in the output, ex: 1,2-,-5,2-5. Fields are 1-based and inclusive.
    #[structopt(short, long)]
    fields: Option<String>,

    /// A regex to select headers, ex: '^is_.*$`.
    #[structopt(short = "F", long)]
    header_fields: Option<Vec<Regex>>,

    /// Treat the header_fields as string literals instead of regex's
    #[structopt(short = "L", long)]
    literal: bool,
}

fn main() -> Result<(), Box<dyn Error>> {
    // TODO: add the complement argument to flip the FieldRange
    // TODO: handle errors and pipe closing more gracefully
    let opts = Opts::from_args();
    run(&opts)?;
    Ok(())
}

/// Run the actual parsing and writing
fn run(opts: &Opts) -> Result<(), Box<dyn Error>> {
    let mut reader = BufReader::new(select_input(opts.input.as_ref())?);
    let mut writer = BufWriter::new(select_output(opts.output.as_ref())?);
    let mut buffer = String::new();
    let mut skip_first_read = false;

    let fields = match (&opts.fields, &opts.header_fields) {
        (Some(field_list), Some(header_fields)) => {
            reader.read_line(&mut buffer)?;
            buffer.pop(); // remove newline
            skip_first_read = true;
            let mut fields = FieldRange::from_list(field_list)?;
            let header_fields = FieldRange::from_header_list(
                header_fields,
                &buffer,
                &opts.delimiter,
                opts.literal,
            )?;
            fields.extend(header_fields.into_iter());
            FieldRange::post_process_ranges(&mut fields);
            fields
        }
        (Some(field_list), None) => FieldRange::from_list(field_list)?,
        (None, Some(header_fields)) => {
            reader.read_line(&mut buffer)?;
            buffer.pop(); // remove newline
            skip_first_read = true;
            FieldRange::from_header_list(header_fields, &buffer, &opts.delimiter, opts.literal)?
        }
        (None, None) => {
            eprintln!("Must select one or both `fields` and 'header-fields`.");
            exit(1);
        }
    };

    // This vec is reused with each pass of the loop. It holds a vec for each FieldRange. Values
    // are pushed onto each FieldRange's vec and then printed at the end of the loop. This allows
    // values to be printed in the order specified by the user since a FieldRange will be push values
    // onto the index indicated by FieldRange.pos.
    //
    // Below, we are creating a new variable in the loop, `staging`, to coerce the `str`'s lifetime
    // to a short lifetime consistent with the lifetime of the loop. Then, after draining the values
    // from the inner vecs we are turning `staging_empty` back into `Vec<Vec<&'static>>`, again by
    // coercion. In theory this should all be zero cost due to
    // [InPlaceIterable](https://doc.rust-lang.org/std/iter/trait.InPlaceIterable.html). Godbolt proves
    // this is so as well. See links in the linked rust-lang thread.
    //
    // See also https://users.rust-lang.org/t/review-of-unsafe-usage/61520
    let mut staging_empty: Vec<Vec<&'static str>> = vec![vec![]; fields.len()];
    loop {
        // If we read the header line then use existing buffer.
        if skip_first_read {
            skip_first_read = false;
        } else {
            if reader.read_line(&mut buffer)? == 0 {
                break;
            }
            // pop the newline off the string
            buffer.pop();
        }

        // Create a lazy splitter
        let mut parts = opts.delimiter.split(&buffer).peekable();
        let mut iterator_index = 0;
        let mut print_delim = false;
        let mut staging = staging_empty;

        // Iterate over our ranges and write any fields that are contained by them.
        for &FieldRange { low, high, pos } in &fields {
            // Advance up to low end of range
            if low > iterator_index {
                match parts.nth(low - iterator_index - 1) {
                    Some(_part) => {
                        iterator_index = low;
                    }
                    None => break,
                }
            }

            // Advance through the range
            for _ in 0..=high - low {
                match parts.next() {
                    Some(part) => {
                        staging[pos].push(part);
                    }
                    None => break,
                }
                iterator_index += 1;
            }
        }

        // Now write the values in the correct order
        // The `collect` calls here should be happening in place resulting in no allocations.
        staging_empty = staging
            .into_iter()
            .map(|mut values| {
                for value in values.drain(..) {
                    if print_delim {
                        write!(&mut writer, "{}", &opts.output_delimiter).unwrap();
                    }
                    print_delim = true;
                    write!(&mut writer, "{}", value).unwrap();
                }
                values.into_iter().map(|_| "").collect()
            })
            .collect();

        // Write endline
        writeln!(&mut writer)?;
        buffer.clear();
    }

    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;
    use tempfile::TempDir;

    #[test]
    #[rustfmt::skip::macros(assert_eq)]
    fn test_parse_fields_good() {
        assert_eq!(vec![FieldRange { low: 0, high: 0, pos: 0}], FieldRange::from_list("1").unwrap());
        assert_eq!(vec![FieldRange { low: 0, high: 0, pos: 0},  FieldRange { low: 3, high: 3, pos: 1}], FieldRange::from_list("1,4").unwrap());
        assert_eq!(vec![FieldRange { low: 0, high: 1, pos: 0},  FieldRange { low: 3, high: usize::MAX - 1, pos: 2}], FieldRange::from_list("1,2,4-").unwrap());
        assert_eq!(vec![FieldRange { low: 0, high: 0, pos: 0},  FieldRange { low: 3, high: usize::MAX - 1, pos: 1}], FieldRange::from_list("1,4-,5-8").unwrap());
        assert_eq!(vec![FieldRange { low: 0, high: 0, pos: 1},  FieldRange { low: 3, high: usize::MAX - 1, pos: 0}, FieldRange { low: 4, high: 7, pos: 2}], FieldRange::from_list("4-,1,5-8").unwrap());
        assert_eq!(vec![FieldRange { low: 0, high: 3, pos: 0}], FieldRange::from_list("-4").unwrap());
        assert_eq!(vec![FieldRange { low: 0, high: 7, pos: 0}], FieldRange::from_list("-4,5-8").unwrap());
    }

    #[test]
    fn test_parse_fields_bad() {
        assert!(FieldRange::from_list("0").is_err());
        assert!(FieldRange::from_list("4-1").is_err());
        assert!(FieldRange::from_list("cat").is_err());
        assert!(FieldRange::from_list("1-dog").is_err());
        assert!(FieldRange::from_list("mouse-4").is_err());
    }

    #[test]
    fn test_parse_header_fields() {
        let header = "is_cat-isdog-wascow-was_is_apple-12345-!$%*(_)";
        let delim = Regex::new("-").unwrap();
        let header_fields = vec![
            Regex::new(r"^is_.*$").unwrap(),
            Regex::new("dog").unwrap(),
            Regex::new(r"\$%").unwrap(),
        ];
        let fields = FieldRange::from_header_list(&header_fields, header, &delim, false).unwrap();
        assert_eq!(
            vec![
                FieldRange {
                    low: 0,
                    high: 1,
                    pos: 0
                },
                FieldRange {
                    low: 5,
                    high: 5,
                    pos: 2 // pos 2 because it's the 3rd regex in header_fields
                }
            ],
            fields
        );
    }

    #[test]
    fn test_parse_header_fields_literal() {
        let header = "is_cat-is-isdog-wascow-was_is_apple-12345-!$%*(_)";
        let delim = Regex::new("-").unwrap();
        let header_fields = vec![
            Regex::new(r"^is_.*$").unwrap(),
            Regex::new("dog").unwrap(),
            Regex::new(r"\$%").unwrap(),
            Regex::new(r"is").unwrap(),
        ];
        let fields = FieldRange::from_header_list(&header_fields, header, &delim, true).unwrap();
        assert_eq!(
            vec![FieldRange {
                low: 1,
                high: 1,
                pos: 3
            },],
            fields
        );
    }

    /// Build a set of opts for testing
    fn build_opts(
        input_file: impl AsRef<Path>,
        output_file: impl AsRef<Path>,
        fields: &str,
    ) -> Opts {
        Opts {
            input: Some(input_file.as_ref().to_path_buf()),
            output: Some(output_file.as_ref().to_path_buf()),
            delimiter: Regex::new(r"\s+").unwrap(),
            output_delimiter: "\t".to_owned(),
            fields: Some(fields.to_owned()),
            header_fields: None,
            literal: false,
        }
    }

    /// Simple function to read a tsv into a nested list of lists.
    fn read_tsv(path: impl AsRef<Path>) -> Vec<Vec<String>> {
        let reader = BufReader::new(File::open(path).unwrap());
        let mut result = vec![];
        for line in reader.lines() {
            let line = line.unwrap();
            result.push(
                line.split('\t')
                    .map(|s| s.to_owned())
                    .collect::<Vec<String>>(),
            )
        }
        result
    }

    /// Write delmited data to a file.
    fn write_file(path: impl AsRef<Path>, data: Vec<Vec<&str>>, sep: &str) {
        let mut writer = BufWriter::new(File::create(path).unwrap());
        for row in data {
            writeln!(&mut writer, "{}", row.join(sep)).unwrap();
        }
        writer.flush().unwrap();
    }

    const FOURSPACE: &str = "    ";

    #[test]
    #[rustfmt::skip::macros(vec)]
    fn test_read_single_values() {
        let tmp = TempDir::new().unwrap();
        let input_file = tmp.path().join("input.txt");
        let output_file = tmp.path().join("output.txt");
        let opts = build_opts(&input_file, &output_file, "1");
        let data = vec![
            vec!["a", "b", "c"],
            vec!["1", "2", "3"],
        ];
        write_file(input_file, data, FOURSPACE);
        run(&opts).unwrap();
        let filtered = read_tsv(output_file);

        assert_eq!(filtered, vec![vec!["a"], vec!["1"]]);
    }

    #[test]
    fn test_read_several_single_values() {
        let tmp = TempDir::new().unwrap();
        let input_file = tmp.path().join("input.txt");
        let output_file = tmp.path().join("output.txt");
        let opts = build_opts(&input_file, &output_file, "1,3");
        let data = vec![vec!["a", "b", "c"], vec!["1", "2", "3"]];
        write_file(input_file, data, FOURSPACE);
        run(&opts).unwrap();
        let filtered = read_tsv(output_file);

        assert_eq!(filtered, vec![vec!["a", "c"], vec!["1", "3"]]);
    }

    #[test]
    fn test_read_single_range() {
        let tmp = TempDir::new().unwrap();
        let input_file = tmp.path().join("input.txt");
        let output_file = tmp.path().join("output.txt");
        let opts = build_opts(&input_file, &output_file, "2-");
        let data = vec![vec!["a", "b", "c", "d"], vec!["1", "2", "3", "4"]];
        write_file(input_file, data, FOURSPACE);
        run(&opts).unwrap();
        let filtered = read_tsv(output_file);

        assert_eq!(filtered, vec![vec!["b", "c", "d"], vec!["2", "3", "4"]]);
    }

    #[test]
    fn test_read_serveral_range() {
        let tmp = TempDir::new().unwrap();
        let input_file = tmp.path().join("input.txt");
        let output_file = tmp.path().join("output.txt");
        let opts = build_opts(&input_file, &output_file, "2-4,6-");
        let data = vec![
            vec!["a", "b", "c", "d", "e", "f", "g"],
            vec!["1", "2", "3", "4", "5", "6", "7"],
        ];
        write_file(input_file, data, FOURSPACE);
        run(&opts).unwrap();
        let filtered = read_tsv(output_file);

        assert_eq!(
            filtered,
            vec![vec!["b", "c", "d", "f", "g"], vec!["2", "3", "4", "6", "7"]]
        );
    }

    #[test]
    fn test_read_mixed_fields1() {
        let tmp = TempDir::new().unwrap();
        let input_file = tmp.path().join("input.txt");
        let output_file = tmp.path().join("output.txt");
        let opts = build_opts(&input_file, &output_file, "2,4-");
        let data = vec![
            vec!["a", "b", "c", "d", "e", "f", "g"],
            vec!["1", "2", "3", "4", "5", "6", "7"],
        ];
        write_file(input_file, data, FOURSPACE);
        run(&opts).unwrap();
        let filtered = read_tsv(output_file);

        assert_eq!(
            filtered,
            vec![vec!["b", "d", "e", "f", "g"], vec!["2", "4", "5", "6", "7"]]
        );
    }

    #[test]
    fn test_read_mixed_fields2() {
        let tmp = TempDir::new().unwrap();
        let input_file = tmp.path().join("input.txt");
        let output_file = tmp.path().join("output.txt");
        let opts = build_opts(&input_file, &output_file, "-4,7");
        let data = vec![
            vec!["a", "b", "c", "d", "e", "f", "g"],
            vec!["1", "2", "3", "4", "5", "6", "7"],
        ];
        write_file(input_file, data, FOURSPACE);
        run(&opts).unwrap();
        let filtered = read_tsv(output_file);

        assert_eq!(
            filtered,
            vec![vec!["a", "b", "c", "d", "g"], vec!["1", "2", "3", "4", "7"]]
        );
    }

    #[test]
    fn test_read_no_delimis_found() {
        let tmp = TempDir::new().unwrap();
        let input_file = tmp.path().join("input.txt");
        let output_file = tmp.path().join("output.txt");
        let opts = build_opts(&input_file, &output_file, "-4,7");
        let data = vec![
            vec!["a", "b", "c", "d", "e", "f", "g"],
            vec!["1", "2", "3", "4", "5", "6", "7"],
        ];
        write_file(input_file, data, "-");
        run(&opts).unwrap();
        let filtered = read_tsv(output_file);

        // We hae no concept of only-delimited, so if no delim is found the whole line
        // is treated as column 1.
        assert_eq!(filtered, vec![vec!["a-b-c-d-e-f-g"], vec!["1-2-3-4-5-6-7"]]);
    }

    #[test]
    fn test_read_over_end() {
        let tmp = TempDir::new().unwrap();
        let input_file = tmp.path().join("input.txt");
        let output_file = tmp.path().join("output.txt");
        let opts = build_opts(&input_file, &output_file, "-4,8,11-");
        let data = vec![
            vec!["a", "b", "c", "d", "e", "f", "g"],
            vec!["1", "2", "3", "4", "5", "6", "7"],
        ];
        write_file(input_file, data, FOURSPACE);
        run(&opts).unwrap();
        let filtered = read_tsv(output_file);

        // columns past end in fields are ignored
        assert_eq!(
            filtered,
            vec![vec!["a", "b", "c", "d"], vec!["1", "2", "3", "4"]]
        );
    }

    #[test]
    fn test_reorder1() {
        let tmp = TempDir::new().unwrap();
        let input_file = tmp.path().join("input.txt");
        let output_file = tmp.path().join("output.txt");
        let opts = build_opts(&input_file, &output_file, "6,-4");
        let data = vec![
            vec!["a", "b", "c", "d", "e", "f", "g"],
            vec!["1", "2", "3", "4", "5", "6", "7"],
        ];
        write_file(input_file, data, FOURSPACE);
        run(&opts).unwrap();
        let filtered = read_tsv(output_file);

        // columns past end in fields are ignored
        assert_eq!(
            filtered,
            vec![vec!["f", "a", "b", "c", "d"], vec!["6", "1", "2", "3", "4"]]
        );
    }

    #[test]
    fn test_reorder2() {
        let tmp = TempDir::new().unwrap();
        let input_file = tmp.path().join("input.txt");
        let output_file = tmp.path().join("output.txt");
        // 4-5 should not be repeated at the end and only written once.
        let opts = build_opts(&input_file, &output_file, "3-,1,4-5");
        let data = vec![
            vec!["a", "b", "c", "d", "e", "f", "g"],
            vec!["1", "2", "3", "4", "5", "6", "7"],
        ];
        write_file(input_file, data, FOURSPACE);
        run(&opts).unwrap();
        let filtered = read_tsv(output_file);

        // columns past end in fields are ignored
        assert_eq!(
            filtered,
            vec![
                vec!["c", "d", "e", "f", "g", "a"],
                vec!["3", "4", "5", "6", "7", "1"]
            ]
        );
    }
}