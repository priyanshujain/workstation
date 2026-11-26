use console::style;
use disk::cleanup::Size;
use disk::util::format_size;

pub fn print(header: &[&str], rows: &[Vec<String>]) {
    let last = header.len() - 1;
    let widths: Vec<usize> = (0..last)
        .map(|column| {
            rows.iter()
                .map(|row| row[column].chars().count())
                .chain([header[column].len()])
                .max()
                .unwrap_or(0)
        })
        .collect();
    let header: Vec<String> = header.iter().map(|cell| cell.to_string()).collect();
    println!("{}", style(line(&header, &widths)).bold());
    for row in rows {
        println!("{}", line(row, &widths));
    }
}

fn line(row: &[String], widths: &[usize]) -> String {
    let cells: Vec<String> = row
        .iter()
        .enumerate()
        .map(|(column, cell)| match widths.get(column) {
            Some(width) => format!("{cell:<width$}"),
            None => cell.clone(),
        })
        .collect();
    cells.join("  ").trim_end().to_string()
}

pub fn size_label(size: Size) -> String {
    match (size.total(), size.bytes()) {
        (Some(bytes), _) => format_size(bytes),
        (None, 0) => "unknown".to_string(),
        (None, bytes) => format!(">= {}", format_size(bytes)),
    }
}
