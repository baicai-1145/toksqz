pub struct DedupResult {
    pub text: String,
    pub collapsed: usize,
}

pub fn deduplicate_lines(text: &str, threshold: usize) -> DedupResult {
    let threshold = threshold.max(2);
    let lines: Vec<&str> = text.split('\n').collect();
    let mut output: Vec<String> = Vec::with_capacity(lines.len());
    let mut collapsed: usize = 0;

    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        let mut run_length = 1usize;
        while index + run_length < lines.len() && lines[index + run_length] == line {
            run_length += 1;
        }

        if !line.trim().is_empty() && run_length >= threshold {
            output.push(line.to_string());
            output.push(format!("[line repeated {}x]", run_length - 1));
            output.push(format!("[rtk:dropped {} repeated lines]", run_length - 1));
            collapsed += run_length - 1;
            index += run_length;
            continue;
        }

        output.push(line.to_string());
        index += 1;
    }

    DedupResult {
        text: output.join("\n"),
        collapsed,
    }
}
