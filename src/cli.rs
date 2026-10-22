use clap::{Parser, Subcommand, ValueEnum};

// Agents read --help to bootstrap usage; keep tips short and examples real.
const HELP_FOOTER: &str = "\
Which creation command:
  prompt    Build explicit musical directions and a generation command (free)
  write     Compose the song — style prompt + lyric skeleton you fill (free)
  generate  Render audio from lyrics you already have (costs credits)
  describe  Render audio from a one-line description; Suno writes the lyrics
  lyrics    Lyrics text only, no audio (free)

Tips:
  • First run: `suno auth --login`, then `suno doctor` to verify the setup
  • Output is a JSON envelope automatically when piped; force with --json
  • `suno write` and `suno lyrics` are free; standard v6 generation is documented as 10 credits for two songs
  • Exit codes: 0 ok, 1 transient (retry), 2 config/auth, 3 bad input, 4 rate limited
  • Config: `suno config path` shows the file; SUNO_* env vars override it
  • Full machine-readable manifest: `suno agent-info | jq`

Examples:
  suno write --genre \"indie rock\" --theme \"late-night city drives\" --vocal male --out song.txt
  # fill the <...> lyric slots in song.txt, then run the generate command `write` printed:
  suno generate --title \"Night Drive\" --tags \"Indie rock, jangly guitars, warm male vocals, 110 BPM\" --lyrics-file song.txt --wait --download ./songs/
    The full flow: compose, fill, render, download (lyrics embedded in the MP3)

  suno describe --prompt \"a chill lo-fi track about rainy mornings\" --wait --download ./
    Let Suno write the lyrics from a description

  suno list | jq -r '.data.clips[].id'
    List your library as JSON and extract clip IDs

  suno timed-lyrics <clip_id> --lrc > song.lrc
    Word-level synced lyrics in LRC format (raw even when piped)

  suno cover <clip_id> --tags \"jazz, smooth piano\" --wait
    Re-imagine an existing clip in a new style";

#[derive(Parser)]
#[command(
    name = "suno",
    version,
    about = "Write, generate, and manage Suno music — v6 support",
    after_long_help = HELP_FOOTER
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,

    /// Output JSON (auto-detected when piped)
    #[arg(long, global = true)]
    pub json: bool,

    /// Suppress non-essential output
    #[arg(long, global = true)]
    pub quiet: bool,

    /// Use invisible Chrome only when a captcha is required; never open a window
    #[arg(long, global = true)]
    pub headless: bool,

    /// Use HTTP only; never access or launch a browser
    #[arg(long, global = true)]
    pub no_browser: bool,
}

// Command order drives `--help` order: the creation commands lead, composer
// first, because `write` is where a song starts.
#[derive(Subcommand)]
pub enum Commands {
    /// Build a musical brief and generation command offline (free)
    Prompt(PromptArgs),

    /// Compose a Suno-ready structured song (the native song generator)
    Write(WriteArgs),

    /// Generate music with custom lyrics, tags, and controls
    Generate(GenerateArgs),

    /// Generate music from a text description (Suno writes lyrics)
    Describe(DescribeArgs),

    /// Generate lyrics only (free, no credits used)
    Lyrics(LyricsArgs),

    /// Continue/extend a clip from a timestamp
    Extend(ExtendArgs),

    /// Concatenate clips into a full song
    Concat(ConcatArgs),

    /// Create a cover of an existing clip
    Cover(CoverArgs),

    /// Remaster a clip with a different model
    Remaster(RemasterArgs),

    /// Extract stems (vocals, instruments) from a clip
    Stems(StemsArgs),

    /// Show detailed info for a single clip
    Info(InfoArgs),

    /// View a voice persona
    Persona(PersonaArgs),

    /// List your songs
    #[command(visible_alias = "ls")]
    List(ListArgs),

    /// Search your songs by title or tags
    Search(SearchArgs),

    /// Check generation status
    Status(StatusArgs),

    /// List saved generation receipts for recovery after interruption
    Jobs(JobsArgs),

    /// Download audio/video for clip(s)
    #[command(visible_alias = "dl")]
    Download(DownloadArgs),

    /// Delete/trash a clip
    #[command(visible_alias = "rm")]
    Delete(DeleteArgs),

    /// Update clip title, lyrics, or caption
    Set(SetArgs),

    /// Toggle clip public/private
    Publish(PublishArgs),

    /// Get word-level timestamped lyrics
    TimedLyrics(TimedLyricsArgs),

    /// Show credit balance and plan info
    Credits,

    /// List available models
    Models,

    /// Set up authentication
    Auth(AuthArgs),

    /// Manage configuration
    Config(ConfigArgs),

    /// Check external dependencies and configuration health
    Doctor,

    /// Machine-readable capabilities (for AI agents)
    AgentInfo(AgentInfoArgs),

    /// Read built-in songwriting guides (list all, or print one)
    #[command(visible_alias = "guides")]
    Guide(GuideArgs),

    /// Manage the agent skill (teaches Claude Code / Codex / Gemini how to use this CLI)
    Skill(SkillArgs),

    /// Back-compat alias for `skill install` (hidden)
    #[command(hide = true)]
    InstallSkill(InstallSkillArgs),

    /// Distribution-aware update check/apply
    Update(UpdateArgs),

    /// Hidden: deterministic exit-code trigger for contract tests
    #[command(hide = true)]
    Contract {
        /// Exit code to trigger (0-4)
        code: i32,
    },
}

#[derive(clap::Args)]
#[command(after_long_help = "Examples:
  suno prompt --preset comic-folk --title 'The Missing Pie' --lyrics-file song.txt
  suno prompt --genre 'chamber folk' --bpm 88 --meter 6/8 --beat-unit dotted-quarter --voice 'adult low alto' --delivery 'dry, deadpan, clipped consonants' --instruments 'bowed fiddle and hand drum'
  suno prompt --list-presets
Read `suno guide prompting` for source-backed advice, original examples, and audition checks. Missing directions are reported, never silently filled. Musical directions guide the model; they do not guarantee measured BPM, key, or casting.")]
pub struct PromptArgs {
    /// Show the original example presets and their full directions
    #[arg(long)]
    pub list_presets: bool,
    /// Start from an example: comic-folk, work-song, solo-lament, electronic
    #[arg(long)]
    pub preset: Option<String>,
    /// Main musical tradition or genre; name the sound, not just the story setting
    #[arg(long)]
    pub genre: Option<String>,
    /// Emotion and energy, e.g. wry and boisterous
    #[arg(long)]
    pub mood: Option<String>,
    /// Target beats per minute; pair compound meters with --beat-unit
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=999))]
    pub bpm: Option<u32>,
    /// Time signature, e.g. 4/4, 3/4, 6/8
    #[arg(long)]
    pub meter: Option<String>,
    /// Note counted by BPM: quarter, dotted-quarter, or eighth
    #[arg(long, value_parser = ["quarter", "dotted-quarter", "eighth"])]
    pub beat_unit: Option<String>,
    /// Rhythm feel, e.g. straight march, swung eighths, two-beat jig
    #[arg(long)]
    pub groove: Option<String>,
    /// Target tonal center or mode, e.g. D Dorian (a direction, not a guarantee)
    #[arg(long)]
    pub key: Option<String>,
    /// Specific voice casting: register, perceived age, texture, solo/group
    #[arg(long, conflicts_with = "instrumental")]
    pub voice: Option<String>,
    /// Performance behavior, e.g. deadpan talk-singing or full-voice calls
    #[arg(long)]
    pub delivery: Option<String>,
    /// Optional Suno vocal-gender control; free-text --voice supplies detail
    #[arg(long, conflicts_with = "instrumental")]
    pub vocal: Option<VocalGender>,
    /// Instruments and their roles; say a cappella for voices alone
    #[arg(long)]
    pub instruments: Option<String>,
    /// Section development, e.g. solo verse, group response, abrupt ending
    #[arg(long)]
    pub arrangement: Option<String>,
    /// Recording/mix direction, e.g. dry room, close vocal, narrow stereo
    #[arg(long)]
    pub production: Option<String>,
    /// Unwanted sounds; kept separate from the positive style prompt
    #[arg(long)]
    pub exclude: Option<String>,
    /// No vocals (different from a cappella, which is voices without instruments)
    #[arg(long)]
    pub instrumental: bool,
    /// Title included in the proposed generation command
    #[arg(long)]
    pub title: Option<String>,
    /// Existing finished lyrics for the proposed generation command
    #[arg(long)]
    pub lyrics_file: Option<String>,
}

#[derive(clap::Args)]
pub struct UpdateArgs {
    /// Check for a new version without installing
    #[arg(long)]
    pub check: bool,

    /// Bypass the duplicate-run guard
    #[arg(long)]
    pub force: bool,
}

#[derive(clap::Args)]
pub struct GuideArgs {
    /// Guide name or alias (e.g. songwriting, priming). Omit to list all guides.
    pub name: Option<String>,
}

// `suno write` help. Agents read --help to learn the command; keep it concrete.
const WRITE_HELP: &str = "\
Tips:
  • --out FILE is the way to get an editable lyrics file: it holds the lyric block ONLY,
    so it feeds `suno generate --lyrics-file` directly. Title/style/tags go to stderr and JSON
  • Shell redirection (`suno write > song.txt`) receives the JSON envelope, not lyrics —
    output is a JSON envelope whenever stdout is not a terminal. Use --out for the lyrics
  • --project-out FILE writes the full human document (title + style prompt + tags + artefact)
  • Fill the <...> placeholders before generating; `suno generate` rejects unresolved ones
  • Genres are fuzzy/case-insensitive; an unknown genre becomes a raw style tag (never fails)
  • --viral adds earworm/hook meta-tags; --instrumental drops all vocals and lyric slots and
    emits a generate command with --instrumental
  • --mode priming requires --target, --objective and --domain (see `suno guide priming`)

Examples:
  suno write --theme \"late-night coding\" --genre indie-rock --vocal male --out song.txt
    Scaffold to song.txt; fill the <...> slots, then run the printed generate command

  suno write --theme \"summer love\" --genre pop --viral --title \"Golden Hour\" --out song.txt
    A pop song with earworm hook tags baked into the style prompt

  suno write --genre lo-fi --instrumental --out beat.txt
    Instrumental lo-fi scaffold — no lyric slots, generatable as written

  suno write --mode priming --domain investment --target \"batch: seed investors\" \\
      --objective \"increase recall of fund X\" --subtlety medium --out song.txt
    Priming research scaffold (chill lounge, 72 BPM) with a Prime-Stack Map template";

#[derive(clap::Args)]
#[command(after_long_help = WRITE_HELP)]
pub struct WriteArgs {
    /// What the song is about (fills the {theme} placeholders)
    #[arg(long)]
    pub theme: Option<String>,

    /// Genre or subgenre (fuzzy match; unknown → used verbatim as a style tag)
    #[arg(long)]
    pub genre: Option<String>,

    /// Mood override, e.g. "bittersweet and hopeful" (else the genre default)
    #[arg(long)]
    pub mood: Option<String>,

    /// Vocal gender direction (male | female)
    #[arg(long)]
    pub vocal: Option<VocalGender>,

    /// Tempo in BPM (defaults to the genre's tempo)
    #[arg(long)]
    pub bpm: Option<u32>,

    /// Add earworm / hook meta-tags and catchiness tags
    #[arg(long)]
    pub viral: bool,

    /// Instrumental — no vocals, no lyric placeholders
    #[arg(long)]
    pub instrumental: bool,

    /// Song title (defaults to a title derived from the theme)
    #[arg(long)]
    pub title: Option<String>,

    /// Composition mode
    #[arg(long, value_enum, default_value_t = WriteMode::Songwriting)]
    pub mode: WriteMode,

    /// [priming] Named consenting target or anonymised batch descriptor
    #[arg(long)]
    pub target: Option<String>,

    /// [priming] Specific, falsifiable priming objective
    #[arg(long)]
    pub objective: Option<String>,

    /// [priming] Domain: investment/marketing/sales/political/health/other
    #[arg(long)]
    pub domain: Option<String>,

    /// [priming] Subtlety dial: stealth/medium/overt
    #[arg(long)]
    pub subtlety: Option<String>,

    /// Write the lyric block to FILE — the file `generate --lyrics-file` reads
    #[arg(long)]
    pub out: Option<String>,

    /// Also write the full project document (title, style prompt, tags,
    /// priming artefact) to FILE. Never a generation input.
    #[arg(long)]
    pub project_out: Option<String>,

    /// Download directory baked into the emitted `suno generate` command
    #[arg(long, default_value = "./")]
    pub download: String,
}

/// Composition modes. Extensible: add a variant here and one match arm in
/// `commands::write` to ship a new mode.
#[derive(ValueEnum, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WriteMode {
    /// The base songwriting grammar scaffold
    #[default]
    Songwriting,
    /// Priming-research scaffold (chill lounge + Prime-Stack Map)
    Priming,
}

impl WriteMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Songwriting => "songwriting",
            Self::Priming => "priming",
        }
    }
}

#[derive(clap::Args)]
pub struct SkillArgs {
    #[command(subcommand)]
    pub action: SkillAction,
}

#[derive(Subcommand)]
pub enum SkillAction {
    /// Write the skill file to all detected agent platforms
    Install,
    /// Check which platforms have the skill installed and current
    Status,
}

#[derive(clap::Args)]
pub struct GenerateArgs {
    /// Song title
    #[arg(short, long)]
    pub title: Option<String>,

    /// Style tags (comma-separated): "pop, synths, upbeat"
    #[arg(long)]
    pub tags: Option<String>,

    /// Exclude styles (comma-separated): "metal, heavy"
    #[arg(long)]
    pub exclude: Option<String>,

    /// Lyrics text (with [Verse], [Chorus] tags)
    #[arg(short, long, conflicts_with = "lyrics_file")]
    pub lyrics: Option<String>,

    /// Read lyrics from file
    #[arg(long)]
    pub lyrics_file: Option<String>,

    /// Model version (default: config `default_model`, v6 out of the box)
    #[arg(short, long)]
    pub model: Option<ModelVersion>,

    /// Vocal gender
    #[arg(long)]
    pub vocal: Option<VocalGender>,

    /// Weirdness level (0-100)
    #[arg(long, value_parser = percentage)]
    pub weirdness: Option<f64>,

    /// Style influence strength (0-100)
    #[arg(long, value_parser = percentage)]
    pub style_influence: Option<f64>,

    /// Audio influence strength (0-100) — how strongly source audio shapes
    /// the output
    #[arg(long, value_parser = percentage)]
    pub audio_influence: Option<f64>,

    /// Generate instrumental only (no vocals)
    #[arg(long)]
    pub instrumental: bool,

    /// Bypass the duplicate-run guard
    #[arg(long)]
    pub force: bool,

    /// Send literal <...> placeholders in lyrics as written
    #[arg(long)]
    pub allow_placeholders: bool,

    /// Wait for generation to complete
    #[arg(short, long)]
    pub wait: bool,

    /// Download output to directory after generation; implies --wait
    #[arg(long)]
    pub download: Option<String>,

    /// hCaptcha token (overrides the auto-solver)
    #[arg(long)]
    pub token: Option<String>,

    /// Captcha provider: 1=hCaptcha, 2=Turnstile (default: Suno preflight)
    #[arg(long, visible_alias = "captcha-provider", value_parser = clap::value_parser!(u8).range(1..=2))]
    pub token_provider: Option<u8>,

    /// Skip the built-in hCaptcha auto-solver. Useful for headless servers
    /// where you supply --token directly (e.g. from a 2Captcha solution).
    #[arg(long)]
    pub no_captcha: bool,

    /// Voice persona ID (generates with your custom voice)
    #[arg(long)]
    pub persona: Option<String>,

    /// Validate and preview the request offline, without authentication or credits
    #[arg(long)]
    pub dry_run: bool,

    /// Ask Suno to spend more compute and credits on this generation
    #[arg(long)]
    pub max_mode: bool,

    /// Stable UUID for retry recovery; a submitted ID returns the existing clips
    #[arg(long)]
    pub request_id: Option<uuid::Uuid>,
}

#[derive(clap::Args)]
pub struct DescribeArgs {
    /// Description of the song you want
    #[arg(short, long)]
    pub prompt: String,

    /// Style tags (optional, guides the generation)
    #[arg(long)]
    pub tags: Option<String>,

    /// Model version (default: config `default_model`, v6 out of the box)
    #[arg(short, long)]
    pub model: Option<ModelVersion>,

    /// Vocal gender
    #[arg(long)]
    pub vocal: Option<VocalGender>,

    /// Weirdness level (0-100)
    #[arg(long, value_parser = percentage)]
    pub weirdness: Option<f64>,

    /// Style influence strength (0-100)
    #[arg(long, value_parser = percentage)]
    pub style_influence: Option<f64>,

    /// Generate instrumental only
    #[arg(long)]
    pub instrumental: bool,

    /// Bypass the duplicate-run guard
    #[arg(long)]
    pub force: bool,

    /// Wait for generation to complete
    #[arg(short, long)]
    pub wait: bool,

    /// Download output to directory; implies --wait
    #[arg(long)]
    pub download: Option<String>,

    /// hCaptcha token (overrides the auto-solver)
    #[arg(long)]
    pub token: Option<String>,

    /// Captcha provider: 1=hCaptcha, 2=Turnstile (default: Suno preflight)
    #[arg(long, visible_alias = "captcha-provider", value_parser = clap::value_parser!(u8).range(1..=2))]
    pub token_provider: Option<u8>,

    /// Skip the built-in hCaptcha auto-solver
    #[arg(long)]
    pub no_captcha: bool,

    /// Voice persona ID (generates with your custom voice)
    #[arg(long)]
    pub persona: Option<String>,

    /// Validate and preview the request offline, without authentication or credits
    #[arg(long)]
    pub dry_run: bool,

    /// Ask Suno to spend more compute and credits on this generation
    #[arg(long)]
    pub max_mode: bool,

    /// Stable UUID for retry recovery; a submitted ID returns the existing clips
    #[arg(long)]
    pub request_id: Option<uuid::Uuid>,
}

#[derive(clap::Args)]
pub struct LyricsArgs {
    /// What the song should be about
    #[arg(short, long)]
    pub prompt: String,
}

#[derive(clap::Args)]
pub struct ExtendArgs {
    /// Clip ID to extend
    pub clip_id: String,

    /// Timestamp in seconds to continue from
    #[arg(long)]
    pub at: f64,

    /// New lyrics for the extension
    #[arg(long)]
    pub lyrics: Option<String>,

    /// Style tags
    #[arg(long)]
    pub tags: Option<String>,

    /// Model version (default: config `default_model`, v6 out of the box)
    #[arg(short, long)]
    pub model: Option<ModelVersion>,

    /// hCaptcha token (overrides the auto-solver)
    #[arg(long)]
    pub token: Option<String>,

    /// Captcha provider: 1=hCaptcha, 2=Turnstile (default: Suno preflight)
    #[arg(long, visible_alias = "captcha-provider", value_parser = clap::value_parser!(u8).range(1..=2))]
    pub token_provider: Option<u8>,

    /// Skip the built-in hCaptcha auto-solver
    #[arg(long)]
    pub no_captcha: bool,

    /// Bypass the duplicate-run guard
    #[arg(long)]
    pub force: bool,

    /// Send literal <...> placeholders in lyrics as written
    #[arg(long)]
    pub allow_placeholders: bool,

    /// Wait for completion
    #[arg(short, long)]
    pub wait: bool,
}

#[derive(clap::Args)]
pub struct ConcatArgs {
    /// Clip ID to concatenate into a full song
    pub clip_id: String,
}

#[derive(clap::Args)]
pub struct CoverArgs {
    /// Clip ID to create a cover of
    pub clip_id: String,

    /// Style tags for the cover
    #[arg(long)]
    pub tags: Option<String>,

    /// Model version for the cover (default: config `default_model`)
    #[arg(short, long)]
    pub model: Option<ModelVersion>,

    /// Audio influence strength (0-100) — how strongly the source clip
    /// shapes the cover
    #[arg(long, value_parser = percentage)]
    pub audio_influence: Option<f64>,

    /// Bypass the duplicate-run guard
    #[arg(long)]
    pub force: bool,

    /// hCaptcha token (overrides the auto-solver)
    #[arg(long)]
    pub token: Option<String>,

    /// Captcha provider: 1=hCaptcha, 2=Turnstile (default: Suno preflight)
    #[arg(long, visible_alias = "captcha-provider", value_parser = clap::value_parser!(u8).range(1..=2))]
    pub token_provider: Option<u8>,

    /// Skip the built-in hCaptcha auto-solver
    #[arg(long)]
    pub no_captcha: bool,

    /// Wait for completion
    #[arg(short, long)]
    pub wait: bool,

    /// Download output to directory; implies --wait
    #[arg(long)]
    pub download: Option<String>,
}

#[derive(clap::Args)]
pub struct RemasterArgs {
    /// Clip ID to remaster
    pub clip_id: String,

    /// Remaster model version
    #[arg(long, default_value = "v6")]
    pub model: RemasterModel,

    /// Bypass the duplicate-run guard
    #[arg(long)]
    pub force: bool,

    /// hCaptcha token (overrides the auto-solver)
    #[arg(long)]
    pub token: Option<String>,

    /// Captcha provider: 1=hCaptcha, 2=Turnstile (default: Suno preflight)
    #[arg(long, visible_alias = "captcha-provider", value_parser = clap::value_parser!(u8).range(1..=2))]
    pub token_provider: Option<u8>,

    /// Skip the built-in hCaptcha auto-solver
    #[arg(long)]
    pub no_captcha: bool,

    /// Wait for completion
    #[arg(short, long)]
    pub wait: bool,

    /// Download output to directory; implies --wait
    #[arg(long)]
    pub download: Option<String>,
}

#[derive(clap::Args)]
pub struct InfoArgs {
    /// Clip ID to inspect
    #[arg(conflicts_with = "command")]
    pub id: Option<String>,

    /// Discover one command; without an ID this command aliases agent-info
    #[arg(long)]
    pub command: Option<String>,
}

#[derive(clap::Args)]
pub struct AgentInfoArgs {
    /// Canonical command or group, e.g. generate or config
    #[arg(long)]
    pub command: Option<String>,
}

#[derive(clap::Args)]
pub struct PersonaArgs {
    /// Persona ID to view
    pub id: String,
}

#[derive(clap::Args)]
pub struct StemsArgs {
    /// Clip ID to extract stems from
    pub clip_id: String,
}

#[derive(clap::Args)]
pub struct ListArgs {
    /// Opaque pagination cursor from a previous `list --json` response
    /// (`next_cursor`). Omit for the first page.
    #[arg(long)]
    pub cursor: Option<String>,
}

#[derive(clap::Args)]
pub struct SearchArgs {
    /// Search query (matches title and tags)
    pub query: String,
}

#[derive(clap::Args)]
pub struct DeleteArgs {
    /// Clip ID(s) to delete
    pub ids: Vec<String>,

    /// Skip confirmation
    #[arg(short = 'y', long)]
    pub yes: bool,

    /// Confirm moving clips to trash (alias of -y/--yes)
    #[arg(long)]
    pub confirm: bool,

    /// Restore the clip(s) from trash instead of trashing them
    #[arg(long)]
    pub restore: bool,
}

#[derive(clap::Args)]
pub struct StatusArgs {
    /// Clip ID(s) to check
    #[arg(required = true, num_args = 1..)]
    pub ids: Vec<String>,

    /// Wait for these existing clips; never submits a new generation
    #[arg(short, long)]
    pub wait: bool,

    /// Download completed clips; implies --wait
    #[arg(long)]
    pub download: Option<String>,
}

#[derive(clap::Args)]
pub struct JobsArgs {
    /// Maximum number of recent receipts
    #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u32).range(1..=1000))]
    pub limit: u32,
}

#[derive(clap::Args)]
pub struct DownloadArgs {
    /// Clip ID(s) to download
    #[arg(required = true, num_args = 1..)]
    pub ids: Vec<String>,

    /// Output directory (default: config `output_dir`, "." out of the box)
    #[arg(short, long)]
    pub output: Option<String>,

    /// Download video instead of audio
    #[arg(long)]
    pub video: bool,

    /// File format; MP3 is the default
    #[arg(long, value_enum, default_value_t = DownloadFormat::Mp3, conflicts_with = "video")]
    pub format: DownloadFormat,

    /// auto selects Studio/library; playback uses metadata URLs (MP3/MP4 only)
    #[arg(long, value_enum, default_value_t = DownloadSource::Auto)]
    pub source: DownloadSource,
}

#[derive(clap::Args)]
pub struct SetArgs {
    /// Clip ID to update
    pub id: String,

    /// New title
    #[arg(long)]
    pub title: Option<String>,

    /// New lyrics text
    #[arg(long)]
    pub lyrics: Option<String>,

    /// Read lyrics from file
    #[arg(long)]
    pub lyrics_file: Option<String>,

    /// New caption
    #[arg(long)]
    pub caption: Option<String>,

    /// Remove custom cover image
    #[arg(long)]
    pub remove_cover: bool,
}

#[derive(clap::Args)]
pub struct PublishArgs {
    /// Clip ID(s)
    #[arg(required = true, num_args = 1..)]
    pub ids: Vec<String>,

    /// Make public (default) or --private
    #[arg(long)]
    pub private: bool,
}

#[derive(clap::Args)]
pub struct TimedLyricsArgs {
    /// Clip ID
    pub id: String,

    /// Output as LRC format
    #[arg(long)]
    pub lrc: bool,
}

#[derive(clap::Args)]
pub struct AuthArgs {
    /// Auto-extract from browser (recommended)
    #[arg(long)]
    pub login: bool,

    /// Force-refresh the JWT via the stored Clerk session cookie. Use this
    /// when the CLI returns `auth_expired` or `Token validation failed`
    /// without requiring a full re-login from the browser.
    #[arg(long)]
    pub refresh: bool,

    /// JWT token (manual fallback)
    #[arg(long)]
    pub jwt: Option<String>,

    /// Clerk __client cookie (manual fallback for headless servers)
    ///
    /// Accepts either the raw __client value or a full browser Cookie header.
    #[arg(long)]
    pub cookie: Option<String>,

    /// Device ID
    #[arg(long)]
    pub device: Option<String>,

    /// Remove stored authentication
    #[arg(long)]
    pub logout: bool,

    /// Read a Clerk cookie from stdin (keeps it out of process arguments)
    #[arg(long, conflicts_with_all = ["cookie", "jwt", "jwt_stdin", "login", "refresh", "logout"])]
    pub cookie_stdin: bool,

    /// Read a JWT from stdin (keeps it out of process arguments)
    #[arg(long, conflicts_with_all = ["cookie", "jwt", "cookie_stdin", "login", "refresh", "logout"])]
    pub jwt_stdin: bool,
}

#[derive(clap::Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub action: ConfigAction,
}

#[derive(clap::Args)]
pub struct InstallSkillArgs {
    /// Custom output path (writes a single SKILL.md there instead of the
    /// per-platform install)
    #[arg(long)]
    pub path: Option<String>,

    /// Rewrite skill files even when already current
    #[arg(short, long)]
    pub force: bool,

    /// Print the skill content to stdout instead of writing
    #[arg(long)]
    pub print: bool,
}

#[derive(Subcommand)]
pub enum ConfigAction {
    /// Show the effective merged configuration (defaults < file < SUNO_* env)
    Show,
    /// Set a configuration value in the config file
    Set { key: String, value: String },
    /// Show the configuration file path
    Path,
    /// Validate the configuration file
    Check,
}

#[derive(ValueEnum, Clone, Debug, Default)]
pub enum ModelVersion {
    #[value(name = "v6")]
    #[default]
    V6,
    #[value(name = "v6-wild")]
    V6Wild,
    #[value(name = "v6-mini")]
    V6Mini,
    #[value(name = "v5.5")]
    V55,
    #[value(name = "v5")]
    V5,
    #[value(name = "v4.5+")]
    V45Plus,
    #[value(name = "v4.5-all")]
    V45All,
    #[value(name = "v4.5")]
    V45,
    #[value(name = "v4")]
    V4,
    #[value(name = "v3.5")]
    V35,
    #[value(name = "v3")]
    V3,
    #[value(name = "v2")]
    V2,
}

impl ModelVersion {
    pub fn to_api_key(&self) -> &'static str {
        match self {
            Self::V6 => "chirp-hawk",
            Self::V6Wild => "chirp-hawk-wild",
            Self::V6Mini => "chirp-goose",
            Self::V55 => "chirp-fenix",
            Self::V5 => "chirp-crow",
            Self::V45Plus => "chirp-bluejay",
            Self::V45All => "chirp-auk-turbo",
            Self::V45 => "chirp-auk",
            Self::V4 => "chirp-v4",
            Self::V35 => "chirp-v3-5",
            Self::V3 => "chirp-v3-0",
            Self::V2 => "chirp-v2-xxl-alpha",
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            Self::V6 => "v6",
            Self::V6Wild => "v6-wild",
            Self::V6Mini => "v6-mini",
            Self::V55 => "v5.5",
            Self::V5 => "v5",
            Self::V45Plus => "v4.5+",
            Self::V45All => "v4.5-all",
            Self::V45 => "v4.5",
            Self::V4 => "v4",
            Self::V35 => "v3.5",
            Self::V3 => "v3",
            Self::V2 => "v2",
        }
    }
}

#[derive(ValueEnum, Clone, Debug)]
pub enum VocalGender {
    Male,
    Female,
}

#[derive(ValueEnum, Clone, Debug, Default)]
pub enum RemasterModel {
    #[value(name = "v6")]
    #[default]
    V6,
    #[value(name = "v5.5")]
    V55,
    #[value(name = "v5")]
    V5,
    #[value(name = "v4.5+")]
    V45Plus,
}

impl RemasterModel {
    pub fn to_api_key(&self) -> &'static str {
        match self {
            Self::V6 => "chirp-halibut",
            Self::V55 => "chirp-flounder",
            Self::V5 => "chirp-carp",
            Self::V45Plus => "chirp-bass",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::ValueEnum;

    #[test]
    fn model_versions_map_to_api_keys() {
        // v4.5-all is the free-tier model missing from the enum until 0.6.0.
        assert_eq!(ModelVersion::V45All.to_api_key(), "chirp-auk-turbo");
        assert_eq!(ModelVersion::V55.to_api_key(), "chirp-fenix");

        // Every selectable --model value must have an API key and a display
        // name matching its clap value name (agent-info relies on this).
        for m in ModelVersion::value_variants() {
            assert!(m.to_api_key().starts_with("chirp"));
            let clap_name = m.to_possible_value().unwrap().get_name().to_string();
            assert_eq!(m.display_name(), clap_name);
        }
    }

    #[test]
    fn remaster_models_map_to_api_keys() {
        for m in RemasterModel::value_variants() {
            assert!(m.to_api_key().starts_with("chirp"));
        }
    }
}

#[derive(ValueEnum, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DownloadFormat {
    #[default]
    Mp3,
    Wav,
    M4a,
    Mp4,
}
impl DownloadFormat {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Mp3 => "mp3",
            Self::Wav => "wav",
            Self::M4a => "m4a",
            Self::Mp4 => "mp4",
        }
    }
}
#[derive(ValueEnum, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DownloadSource {
    #[default]
    Auto,
    Studio,
    Library,
    Playback,
}
fn percentage(value: &str) -> Result<f64, String> {
    let n: f64 = value
        .parse()
        .map_err(|_| "expected a number from 0 to 100")?;
    if n.is_finite() && (0.0..=100.0).contains(&n) {
        Ok(n)
    } else {
        Err("expected a finite number from 0 to 100".into())
    }
}
