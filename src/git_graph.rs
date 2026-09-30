//! Commit graph shown in the editor area: history of every branch, remote and tag with lanes,
//! the selected commit's message and files, and each file's change in the diff view.
//! History is read with the real `git` ([`cli`]), like the rest of the source control panel.

use iced::widget::{button, canvas, column, container, row, scrollable, text, Space};
use iced::{Color, Element, Length, Point};
use std::path::{Path, PathBuf};

use crate::git::cli::{self, Access};

/// Commits loaded at first and added by each "Load more".
pub const PAGE: usize = 500;
const ROW_HEIGHT: f32 = 24.0;
const LANE_WIDTH: f32 = 14.0;

#[derive(Debug, Clone, PartialEq)]
pub struct Commit {
    pub hash: String,
    pub parents: Vec<String>,
    pub author: String,
    /// Unix seconds; 0 when unknown.
    pub date: u64,
    /// Branch and tag names pointing here, as `git log --format=%D` lists them.
    pub refs: Vec<String>,
    pub subject: String,
}

impl Commit {
    fn short(&self) -> &str { &self.hash[..self.hash.len().min(7)] }
}

/// How one row's lanes are drawn: lanes passing by, lanes ending at the commit (its
/// children) and lanes leaving it (its parents). Indices are lane columns.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Lanes {
    pub node: usize,
    pub through: Vec<usize>,
    pub incoming: Vec<usize>,
    pub outgoing: Vec<usize>,
    pub width: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CommitFile {
    /// `M`, `A`, `D`, `R`, ... as `git diff-tree --name-status` reports.
    pub status: String,
    pub path: String,
    /// The path before a rename or copy.
    pub original: Option<String>,
}

pub struct Details {
    pub hash: String,
    pub message: String,
    pub files: Vec<CommitFile>,
}

pub struct Graph {
    pub root: PathBuf,
    commits: Vec<Commit>,
    lanes: Vec<Lanes>,
    limit: usize,
    /// More history exists past `limit`.
    more: bool,
    loading: bool,
    error: Option<String>,
    selected: Option<String>,
    details: Option<Result<Details, String>>,
}

#[derive(Debug, Clone)]
pub enum Message {
    Loaded(Result<(Vec<Commit>, bool), String>),
    LoadMore,
    Select(String),
    DetailsLoaded(String, Result<(String, Vec<CommitFile>), String>),
    /// Show this file's change in the selected commit (handled by the app).
    OpenFile(String, CommitFile),
    Close,
}

impl Graph {
    /// A graph for `root`, with the task that loads its history.
    pub fn open(root: PathBuf) -> (Self, iced::Task<Message>) {
        let mut graph = Self { root, commits: Vec::new(), lanes: Vec::new(), limit: PAGE, more: false, loading: false, error: None, selected: None, details: None };
        let task = graph.reload();
        (graph, task)
    }

    /// Reloads history, e.g. after the repository changed; the selection is kept by hash.
    pub fn reload(&mut self) -> iced::Task<Message> {
        self.loading = true;
        let (root, limit) = (self.root.clone(), self.limit);
        iced::Task::perform(async move {
            tokio::task::spawn_blocking(move || load(&root, limit)).await.map_err(|err| err.to_string())?
        }, Message::Loaded)
    }
}

pub fn update(graph: &mut Graph, message: Message) -> iced::Task<Message> {
    match message {
        Message::Loaded(result) => {
            graph.loading = false;
            match result {
                Ok((commits, more)) => {
                    graph.lanes = layout(&commits);
                    graph.commits = commits;
                    graph.more = more;
                    graph.error = None;
                    if let Some(hash) = graph.selected.clone() {
                        if graph.commits.iter().any(|commit| commit.hash == hash) { return select(graph, hash); }
                        graph.selected = None;
                        graph.details = None;
                    }
                }
                Err(err) => graph.error = Some(err),
            }
        }
        Message::LoadMore => {
            if graph.loading || !graph.more { return iced::Task::none(); }
            graph.limit += PAGE;
            return graph.reload();
        }
        Message::Select(hash) => return select(graph, hash),
        Message::DetailsLoaded(hash, result) => {
            if graph.selected.as_deref() == Some(hash.as_str()) {
                graph.details = Some(result.map(|(message, files)| Details { hash, message, files }));
            }
        }
        Message::OpenFile(..) | Message::Close => {}
    }
    iced::Task::none()
}

fn select(graph: &mut Graph, hash: String) -> iced::Task<Message> {
    let Some(commit) = graph.commits.iter().find(|commit| commit.hash == hash) else { return iced::Task::none() };
    let (root, parent) = (graph.root.clone(), commit.parents.first().cloned());
    graph.selected = Some(hash.clone());
    graph.details = None;
    iced::Task::perform(async move {
        let loaded = hash.clone();
        let result = tokio::task::spawn_blocking(move || details(&root, &loaded, parent.as_deref())).await.map_err(|err| err.to_string());
        (hash, result.and_then(|result| result))
    }, |(hash, result)| Message::DetailsLoaded(hash, result))
}

/// Field separator in `git log` output; commit subjects cannot contain it in practice.
const SEP: char = '\x1f';

/// The newest `limit` commits of every branch, remote branch, tag and `HEAD`, children
/// before parents, and whether there are more.
pub fn load(root: &Path, limit: usize) -> Result<(Vec<Commit>, bool), String> {
    let count = format!("-n{}", limit + 1);
    let mut args = vec!["log", "--topo-order", "--no-color", &count, "--format=%H%x1f%P%x1f%an%x1f%at%x1f%D%x1f%s", "--branches", "--remotes", "--tags"];
    // An unborn `HEAD` would make `git log` fail; a detached one needs naming to be shown.
    let head = cli::run(root, &["rev-parse", "--verify", "-q", "HEAD"], None, Access::Read).is_ok();
    if head { args.push("HEAD"); }
    let output = String::from_utf8_lossy(&cli::run(root, &args, None, Access::Read)?).into_owned();
    let mut commits = parse_log(&output);
    let more = commits.len() > limit;
    commits.truncate(limit);
    Ok((commits, more))
}

fn parse_log(output: &str) -> Vec<Commit> {
    output.lines().filter_map(|line| {
        let mut fields = line.splitn(6, SEP);
        let hash = fields.next().filter(|hash| !hash.is_empty())?.to_owned();
        let parents = fields.next()?.split_whitespace().map(str::to_owned).collect();
        let author = fields.next()?.to_owned();
        let date = fields.next()?.parse().unwrap_or(0);
        let refs = fields.next()?.split(", ").filter(|name| !name.is_empty()).map(str::to_owned).collect();
        Some(Commit { hash, parents, author, date, refs, subject: fields.next().unwrap_or_default().to_owned() })
    }).collect()
}

/// Lane columns for commits listed children first. Each lane waits for one commit; a commit
/// takes the leftmost lane waiting for it (or a free one), ends the other lanes waiting for
/// it, and hands its lane to its first parent. Further parents join a lane already waiting
/// for them, or open a new one.
pub fn layout(commits: &[Commit]) -> Vec<Lanes> {
    let mut lanes: Vec<Option<&str>> = Vec::new();
    commits.iter().map(|commit| {
        let before = lanes.clone();
        let waiting = |lane: &Option<&str>| *lane == Some(commit.hash.as_str());
        let incoming: Vec<usize> = before.iter().enumerate().filter(|(_, lane)| waiting(lane)).map(|(i, _)| i).collect();
        let through: Vec<usize> = before.iter().enumerate().filter(|(_, lane)| lane.is_some() && !waiting(lane)).map(|(i, _)| i).collect();
        let node = incoming.first().copied().unwrap_or_else(|| free_lane(&mut lanes));
        for &lane in &incoming { lanes[lane] = None; }
        let mut outgoing = Vec::new();
        for (index, parent) in commit.parents.iter().enumerate() {
            let lane = if index == 0 {
                node
            } else if let Some(lane) = lanes.iter().position(|lane| *lane == Some(parent.as_str())) {
                outgoing.push(lane);
                continue;
            } else {
                free_lane(&mut lanes)
            };
            lanes[lane] = Some(parent.as_str());
            outgoing.push(lane);
        }
        while lanes.last() == Some(&None) { lanes.pop(); }
        let width = before.len().max(lanes.len()).max(node + 1);
        Lanes { node, through, incoming, outgoing, width }
    }).collect()
}

fn free_lane(lanes: &mut Vec<Option<&str>>) -> usize {
    lanes.iter().position(Option::is_none).unwrap_or_else(|| { lanes.push(None); lanes.len() - 1 })
}

/// The full message and changed files of `hash`, compared with its first parent (so a merge
/// shows what it brought into that branch).
fn details(root: &Path, hash: &str, parent: Option<&str>) -> Result<(String, Vec<CommitFile>), String> {
    let message = String::from_utf8_lossy(&cli::run(root, &["show", "-s", "--format=%B", hash], None, Access::Read)?).trim_end().to_owned();
    let mut args = vec!["diff-tree", "-r", "-M", "--name-status", "-z", "--no-commit-id"];
    match parent {
        Some(parent) => args.extend([parent, hash]),
        None => args.extend(["--root", hash]),
    }
    let output = cli::run(root, &args, None, Access::Read)?;
    Ok((message, parse_name_status(&String::from_utf8_lossy(&output))))
}

/// `git diff-tree --name-status -z` output: status, then one path, or two for renames/copies.
fn parse_name_status(output: &str) -> Vec<CommitFile> {
    let mut fields = output.split('\0').filter(|field| !field.is_empty());
    let mut files = Vec::new();
    while let (Some(status), Some(first)) = (fields.next(), fields.next()) {
        let file = if status.starts_with(['R', 'C']) {
            let Some(path) = fields.next() else { break };
            CommitFile { status: status[..1].to_owned(), path: path.to_owned(), original: Some(first.to_owned()) }
        } else {
            CommitFile { status: status.to_owned(), path: first.to_owned(), original: None }
        };
        files.push(file);
    }
    files
}

/// The patch for `file` in commit `hash`, in the text form the diff view renders.
pub fn file_patch(root: &Path, hash: &str, parent: Option<&str>, file: &CommitFile) -> Result<String, String> {
    let mut args = vec!["--literal-pathspecs", "diff", "--no-ext-diff", "--no-color", "-M"];
    let empty;
    match parent {
        Some(parent) => args.extend([parent, hash]),
        None => {
            // A root commit is compared with the empty tree, whose id depends on the hash format.
            empty = String::from_utf8_lossy(&cli::run(root, &["hash-object", "-t", "tree", "--stdin"], Some(b""), Access::Read)?).trim().to_owned();
            args.extend([empty.as_str(), hash]);
        }
    }
    args.push("--");
    args.extend(file.original.as_deref());
    args.push(&file.path);
    let patch = String::from_utf8_lossy(&cli::run(root, &args, None, Access::Read)?).into_owned();
    Ok(if patch.trim().is_empty() { "No text changes in this file.".into() } else { patch })
}

impl Graph {
    pub fn parent_of(&self, hash: &str) -> Option<String> {
        self.commits.iter().find(|commit| commit.hash == hash)?.parents.first().cloned()
    }
}

const COLORS: [Color; 8] = [
    Color::from_rgb(0.31, 0.64, 1.0),
    Color::from_rgb(0.90, 0.63, 0.29),
    Color::from_rgb(0.36, 0.76, 0.42),
    Color::from_rgb(0.82, 0.43, 0.84),
    Color::from_rgb(0.88, 0.38, 0.37),
    Color::from_rgb(0.24, 0.78, 0.76),
    Color::from_rgb(0.79, 0.75, 0.29),
    Color::from_rgb(0.55, 0.55, 1.0),
];

fn lane_color(lane: usize) -> Color { COLORS[lane % COLORS.len()] }
fn lane_x(lane: usize) -> f32 { LANE_WIDTH / 2.0 + lane as f32 * LANE_WIDTH }

struct LaneCanvas {
    lanes: Lanes,
    merge: bool,
}

impl<M> canvas::Program<M> for LaneCanvas {
    type State = ();
    fn draw(&self, _: &(), renderer: &iced::Renderer, theme: &iced::Theme, bounds: iced::Rectangle, _: iced::mouse::Cursor) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let (mid, bottom) = (bounds.height / 2.0, bounds.height);
        let lanes = &self.lanes;
        let node = Point::new(lane_x(lanes.node), mid);
        let stroke = |lane: usize| canvas::Stroke::default().with_width(2.0).with_color(lane_color(lane));
        for &lane in &lanes.through {
            frame.stroke(&canvas::Path::line(Point::new(lane_x(lane), 0.0), Point::new(lane_x(lane), bottom)), stroke(lane));
        }
        for &lane in &lanes.incoming {
            let from = Point::new(lane_x(lane), 0.0);
            frame.stroke(&canvas::Path::new(|path| { path.move_to(from); path.quadratic_curve_to(Point::new(from.x, mid), node); }), stroke(lane));
        }
        for &lane in &lanes.outgoing {
            let to = Point::new(lane_x(lane), bottom);
            frame.stroke(&canvas::Path::new(|path| { path.move_to(node); path.quadratic_curve_to(Point::new(to.x, mid), to); }), stroke(lane));
        }
        let color = lane_color(lanes.node);
        if self.merge {
            frame.fill(&canvas::Path::circle(node, 4.5), theme.extended_palette().background.base.color);
            frame.stroke(&canvas::Path::circle(node, 4.0), canvas::Stroke::default().with_width(2.0).with_color(color));
        } else {
            frame.fill(&canvas::Path::circle(node, 4.5), color);
        }
        vec![frame.into_geometry()]
    }
}

pub fn view<'a>(graph: &'a Graph) -> Element<'a, Message> {
    let secondary = iced::widget::text::secondary;
    let now = crate::ai_history::now();
    let graph_width = graph.lanes.iter().map(|lanes| lanes.width).max().unwrap_or(1).min(24) as f32 * LANE_WIDTH;
    let mut list = column![];
    for (commit, lanes) in graph.commits.iter().zip(&graph.lanes) {
        let mut labels = row![].spacing(4).align_y(iced::Alignment::Center);
        for name in &commit.refs {
            let head = name.starts_with("HEAD");
            let name = name.strip_prefix("HEAD -> ").or_else(|| name.strip_prefix("tag: ")).unwrap_or(name);
            labels = labels.push(container(text(name.to_owned()).size(11)).padding([1, 5]).style(move |theme: &iced::Theme| {
                let palette = theme.extended_palette();
                let pair = if head { palette.primary.weak } else { palette.background.strong };
                container::Style { background: Some(pair.color.into()), text_color: Some(pair.text), border: iced::Border::default().rounded(3), ..Default::default() }
            }));
        }
        let selected = graph.selected.as_deref() == Some(commit.hash.as_str());
        let lanes_canvas = canvas::Canvas::new(LaneCanvas { lanes: lanes.clone(), merge: commit.parents.len() > 1 })
            .width(Length::Fixed(graph_width)).height(Length::Fixed(ROW_HEIGHT));
        let line = row![
            lanes_canvas,
            container(labels.push(text(commit.subject.clone()).size(13).wrapping(iced::widget::text::Wrapping::None)))
                .width(Length::Fill).clip(true),
            container(text(commit.author.clone()).size(12).style(secondary).wrapping(iced::widget::text::Wrapping::None)).width(140).clip(true),
            container(text(crate::ai_history::ago(now, commit.date)).size(12).style(secondary)).width(44),
            text(commit.short().to_owned()).size(12).font(iced::Font::MONOSPACE).style(secondary),
        ].spacing(10).align_y(iced::Alignment::Center).height(ROW_HEIGHT);
        list = list.push(button(line).padding([0, 8]).width(Length::Fill)
            .style(move |theme, status| {
                let mut style = crate::flat_button_style(theme, status);
                if selected {
                    style.background = Some(theme.extended_palette().primary.weak.color.into());
                    style.text_color = theme.extended_palette().primary.weak.text;
                }
                style
            })
            .on_press(Message::Select(commit.hash.clone())));
    }
    if graph.more {
        list = list.push(container(button(text(if graph.loading { "Loading…" } else { "Load more" }).size(12)).style(crate::flat_button_style)
            .on_press_maybe((!graph.loading).then_some(Message::LoadMore))).padding(8));
    }
    let status = if let Some(err) = &graph.error {
        err.clone()
    } else if graph.loading && graph.commits.is_empty() {
        "Loading history…".into()
    } else if graph.commits.is_empty() {
        "No commits yet".into()
    } else {
        format!("{}{} commits", graph.commits.len(), if graph.more { "+" } else { "" })
    };
    let header = row![
        text("Git Graph").size(14),
        text(status).size(12).style(if graph.error.is_some() { iced::widget::text::danger } else { secondary }),
        button("×").style(crate::flat_button_style).on_press(Message::Close),
        Space::new().width(Length::Fill),
    ].spacing(12).padding(8).align_y(iced::Alignment::Center);
    let mut body = column![header, scrollable(list).height(Length::Fill)];
    if graph.selected.is_some() {
        body = body.push(iced::widget::rule::horizontal(1)).push(container(details_view(graph)).height(220).padding(8));
    }
    body.width(Length::Fill).height(Length::Fill).into()
}

fn details_view(graph: &Graph) -> Element<'_, Message> {
    let secondary = iced::widget::text::secondary;
    let details = match &graph.details {
        None => return text("Loading commit…").size(12).style(secondary).into(),
        Some(Err(err)) => return text(err.clone()).size(12).style(iced::widget::text::danger).into(),
        Some(Ok(details)) => details,
    };
    let commit = graph.commits.iter().find(|commit| commit.hash == details.hash);
    let heading = commit.map(|commit| format!("{} · {} · {}", commit.short(), commit.author, crate::ai_history::ago(crate::ai_history::now(), commit.date)))
        .unwrap_or_default();
    let message = column![
        text(heading).size(12).style(secondary),
        text(details.message.clone()).size(13),
    ].spacing(6);
    let mut files = column![text(format!("{} changed file(s)", details.files.len())).size(12).style(secondary)].spacing(1);
    for file in &details.files {
        let label = match &file.original {
            Some(original) => format!("{original} → {}", file.path),
            None => file.path.clone(),
        };
        files = files.push(button(row![
            text(file.status.clone()).size(12).font(iced::Font::MONOSPACE).width(16),
            text(label).size(12),
        ].spacing(8)).padding([2, 6]).width(Length::Fill).style(crate::flat_button_style)
            .on_press(Message::OpenFile(details.hash.clone(), file.clone())));
    }
    row![
        scrollable(message).width(Length::FillPortion(1)).height(Length::Fill),
        scrollable(files).width(Length::FillPortion(1)).height(Length::Fill),
    ].spacing(16).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(hash: &str, parents: &[&str]) -> Commit {
        Commit { hash: hash.into(), parents: parents.iter().map(|p| p.to_string()).collect(), author: String::new(), date: 0, refs: Vec::new(), subject: String::new() }
    }

    fn lanes(node: usize, through: &[usize], incoming: &[usize], outgoing: &[usize], width: usize) -> Lanes {
        Lanes { node, through: through.to_vec(), incoming: incoming.to_vec(), outgoing: outgoing.to_vec(), width }
    }

    #[test]
    fn lanes_follow_branches_and_merges() {
        // m merges feature (f) into main (b); both branched from a.
        //   m        * lane 0, parents b (lane 0) and f (new lane 1)
        //   f        | * lane 1
        //   b        * | lane 0
        //   a        * lane 0, where lane 1 ends too
        let commits = [commit("m", &["b", "f"]), commit("f", &["a"]), commit("b", &["a"]), commit("a", &[])];
        assert_eq!(layout(&commits), [
            lanes(0, &[], &[], &[0, 1], 2),
            lanes(1, &[0], &[1], &[1], 2),
            lanes(0, &[1], &[0], &[0], 2),
            lanes(0, &[], &[0, 1], &[], 2),
        ]);
    }

    #[test]
    fn lanes_for_linear_history_unrelated_tips_and_reused_columns() {
        let linear = [commit("c", &["b"]), commit("b", &["a"]), commit("a", &[])];
        assert!(layout(&linear).iter().all(|lanes| lanes.node == 0 && lanes.width == 1));
        // Two tips with no shared history, then a lane freed and reused.
        let commits = [commit("x", &["y"]), commit("p", &[]), commit("y", &[]), commit("q", &["r"]), commit("r", &[])];
        let layout = layout(&commits);
        assert_eq!(layout[0], lanes(0, &[], &[], &[0], 1));
        assert_eq!(layout[1], lanes(1, &[0], &[], &[], 2), "a tip opens a new lane beside the waiting one");
        assert_eq!(layout[2], lanes(0, &[], &[0], &[], 1));
        assert_eq!(layout[3], lanes(0, &[], &[], &[0], 1), "the freed first lane is reused");
        // A parent outside the loaded page keeps its lane open to the bottom.
        assert_eq!(super::layout(&[commit("z", &["unloaded"])])[0].outgoing, [0]);
    }

    #[test]
    fn log_and_name_status_output_are_parsed() {
        let log = "aaa\x1fbbb ccc\x1fAnn\x1f1700000000\x1fHEAD -> main, origin/main, tag: v1\x1fMerge branch 'x'\n\
                   bbb\x1f\x1fBob\x1f1690000000\x1f\x1fFirst: a\x1fb";
        let commits = parse_log(log);
        assert_eq!(commits[0].parents, ["bbb", "ccc"]);
        assert_eq!(commits[0].refs, ["HEAD -> main", "origin/main", "tag: v1"]);
        assert_eq!(commits[1], Commit { hash: "bbb".into(), parents: vec![], author: "Bob".into(), date: 1_690_000_000, refs: vec![], subject: "First: a\x1fb".into() });
        assert!(parse_log("").is_empty());

        let files = parse_name_status("M\0src/a b.rs\0R087\0old.rs\0new.rs\0D\0gone.txt\0");
        assert_eq!(files, [
            CommitFile { status: "M".into(), path: "src/a b.rs".into(), original: None },
            CommitFile { status: "R".into(), path: "new.rs".into(), original: Some("old.rs".into()) },
            CommitFile { status: "D".into(), path: "gone.txt".into(), original: None },
        ]);
    }

    #[test]
    fn history_details_and_patches_come_from_git() {
        let git = |root: &Path, args: &[&str]| String::from_utf8(cli::run(root, args, None, Access::Write).unwrap()).unwrap();
        let commit = |root: &Path, message: &str| git(root, &["-c", "user.name=Test", "-c", "user.email=test@example.com", "-c", "commit.gpgsign=false", "commit", "-q", "-m", message]);
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-q"]);
        assert_eq!(load(root, 10).unwrap(), (Vec::new(), false), "no commits yet");
        git(root, &["switch", "-q", "-c", "main"]);
        std::fs::write(root.join("a.txt"), "one\n").unwrap();
        git(root, &["add", "a.txt"]);
        commit(root, "Initial\n\nWith a body.");
        git(root, &["switch", "-q", "-c", "feature"]);
        git(root, &["mv", "a.txt", "renamed.txt"]);
        commit(root, "Rename");
        git(root, &["switch", "-q", "main"]);
        std::fs::write(root.join("b.txt"), "two\n").unwrap();
        git(root, &["add", "b.txt"]);
        commit(root, "Add b");
        git(root, &["-c", "user.name=Test", "-c", "user.email=test@example.com", "-c", "commit.gpgsign=false", "merge", "-q", "--no-ff", "-m", "Merge feature", "feature"]);
        git(root, &["tag", "v1"]);

        let (commits, more) = load(root, 10).unwrap();
        assert!(!more);
        let subjects: Vec<_> = commits.iter().map(|c| c.subject.as_str()).collect();
        assert_eq!(subjects.len(), 4);
        assert_eq!(subjects[0], "Merge feature");
        assert_eq!(subjects[3], "Initial");
        assert!(commits[0].refs.iter().any(|name| name == "HEAD -> main"));
        assert!(commits[0].refs.iter().any(|name| name == "tag: v1"));
        assert_eq!(commits[0].parents.len(), 2);
        assert!(commits.iter().all(|c| c.author == "Test" && c.date > 0));
        assert_eq!(load(root, 2).unwrap().0.len(), 2);
        assert!(load(root, 2).unwrap().1, "more history past the limit");

        let merge = &commits[0];
        let (message, files) = details(root, &merge.hash, merge.parents.first().map(String::as_str)).unwrap();
        assert_eq!(message, "Merge feature");
        assert_eq!(files, [CommitFile { status: "R".into(), path: "renamed.txt".into(), original: Some("a.txt".into()) }]);

        let initial = commits.iter().find(|c| c.subject == "Initial").unwrap();
        let (message, files) = details(root, &initial.hash, None).unwrap();
        assert_eq!(message, "Initial\n\nWith a body.");
        assert_eq!(files, [CommitFile { status: "A".into(), path: "a.txt".into(), original: None }]);
        let patch = file_patch(root, &initial.hash, None, &files[0]).unwrap();
        assert!(patch.contains("+one"), "{patch}");

        let add_b = commits.iter().find(|c| c.subject == "Add b").unwrap();
        let (_, files) = details(root, &add_b.hash, add_b.parents.first().map(String::as_str)).unwrap();
        let patch = file_patch(root, &add_b.hash, add_b.parents.first().map(String::as_str), &files[0]).unwrap();
        assert!(patch.contains("+two") && !patch.contains("one"), "{patch}");
    }
}
