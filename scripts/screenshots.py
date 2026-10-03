#!/usr/bin/env python3
"""Drive the real gh-pulse binary in a pty (pyte) against public repos and render docs/img/*.svg|png.

Usage: GH_PULSE_SHOT_BLOCKLIST=a,b scripts/screenshots.py [--fake] [shot ...]   (needs `pip install pyte`,
`cargo build --release`; PNG conversion uses rsvg-convert if present). Read-only: never confirms an action.
The header login becomes `you`, the unread badge is blanked, and a shot is aborted if a blocklisted string shows.
`--fake` renders the global-home shots (11-17) offline against scripts/fake-gh, a `gh` stub with synthetic data,
in a throwaway HOME/config/state; your own account is never read.
"""
import fcntl, html, os, pty, re, select, shutil, signal, struct, subprocess, sys, tempfile, termios, time
import pyte

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = os.path.join(ROOT, 'target/release/gh-pulse')
OUT = os.path.join(ROOT, 'docs/img')
COLS, ROWS = 140, 40
FONT = "JetBrains Mono, Menlo, Consolas, 'DejaVu Sans Mono', monospace"
CW, CH = 8.4, 18  # cell size
DEF_FG, DEF_BG = '#c9d1d9', '#0d1117'
ANSI = ['black', 'red', 'green', 'brown', 'blue', 'magenta', 'cyan', 'white']
PAL = ['#484f58', '#ff7b72', '#3fb950', '#d29922', '#58a6ff', '#bc8cff', '#39c5cf', '#b1bac4',
       '#6e7681', '#ffa198', '#56d364', '#e3b341', '#79c0ff', '#d2a8ff', '#56d4dd', '#f0f6fc']
NAMED = {n: PAL[i] for i, n in enumerate(ANSI)}
NAMED.update({'brightblack': PAL[8], 'brightred': PAL[9], 'brightgreen': PAL[10], 'brightbrown': PAL[11],
              'brightblue': PAL[12], 'brightmagenta': PAL[13], 'brightcyan': PAL[14], 'brightwhite': PAL[15]})
# screens are aborted if any of these appear (case-insensitive); add your own via GH_PULSE_SHOT_BLOCKLIST=a,b,c
SECRETS = ['/Users/', '@gmail'] + [x for x in os.environ.get('GH_PULSE_SHOT_BLOCKLIST', '').split(',') if x]


def _disp(self):
    return [''.join((self.buffer[y][x].data or ' ') for x in range(self.columns)) for y in range(self.lines)]
pyte.Screen.display = property(_disp)


class T:
    def __init__(s, args, env, cwd=None, clean=False):
        s.scr = pyte.Screen(COLS, ROWS); s.st = pyte.ByteStream(s.scr)
        e = dict(env, TERM='xterm-256color', COLORTERM='truecolor') if clean else dict(os.environ, TERM='xterm-256color', COLORTERM='truecolor', **env)
        s.pid, s.fd = pty.fork()
        if s.pid == 0:
            if cwd: os.chdir(cwd)
            os.execve(BIN, [BIN] + args, e)
        fcntl.ioctl(s.fd, termios.TIOCSWINSZ, struct.pack('HHHH', ROWS, COLS, 0, 0))
        os.kill(s.pid, signal.SIGWINCH)

    def pump(s, t=1.0):
        end = time.time() + t
        while time.time() < end:
            if select.select([s.fd], [], [], 0.1)[0]:
                try: d = os.read(s.fd, 65536)
                except OSError: return
                if not d: return
                s.st.feed(d)

    def send(s, k, t=0.8):
        os.write(s.fd, k.encode()); s.pump(t)

    def text(s): return '\n'.join(s.scr.display)

    def wait_for(s, pat, n=30):
        for _ in range(n):
            if re.search(pat, s.text()): return True
            s.pump(1)
        raise SystemExit(f'timeout waiting for {pat!r}\n{s.text()}')

    def close(s):
        try: os.kill(s.pid, signal.SIGKILL); os.close(s.fd); os.waitpid(s.pid, os.WNOHANG)
        except OSError: pass


def color(c, default):
    if c == 'default': return default
    if c in NAMED: return NAMED[c]
    if re.fullmatch(r'[0-9a-f]{6}', c): return '#' + c
    return default


def _bg(ch): return color(ch.fg if ch.reverse else ch.bg, DEF_FG if ch.reverse else DEF_BG)
def _sty(ch): return (ch.fg, ch.bg, ch.reverse, ch.bold, ch.italics, ch.underscore)


def render_svg(t, path, title):
    scr = t.scr
    W, H = COLS * CW + 32, ROWS * CH + 32 + 28
    o = [f'<svg xmlns="http://www.w3.org/2000/svg" width="{W:.0f}" height="{H:.0f}" viewBox="0 0 {W:.0f} {H:.0f}" font-family="{FONT}" font-size="14">',
         f'<rect width="{W:.0f}" height="{H:.0f}" rx="10" fill="{DEF_BG}"/>',
         f'<rect width="{W:.0f}" height="28" rx="10" fill="#161b22"/><rect y="18" width="{W:.0f}" height="10" fill="#161b22"/>',
         '<circle cx="18" cy="14" r="6" fill="#ff5f56"/><circle cx="38" cy="14" r="6" fill="#ffbd2e"/><circle cx="58" cy="14" r="6" fill="#27c93f"/>',
         f'<text x="{W/2:.0f}" y="19" text-anchor="middle" font-size="12" fill="#8b949e">{html.escape(title)}</text>',
         '<g transform="translate(16,44)">']
    for y in range(ROWS):  # backgrounds first, merged into runs
        row = scr.buffer[y]; x = 0
        while x < COLS:
            bg = _bg(row[x]); x2 = x
            while x2 + 1 < COLS and _bg(row[x2 + 1]) == bg: x2 += 1
            if bg != DEF_BG:
                o.append(f'<rect x="{x*CW:.1f}" y="{y*CH}" width="{(x2-x+1)*CW+0.5:.1f}" height="{CH}" fill="{bg}"/>')
            x = x2 + 1
    for y in range(ROWS):  # text runs of equal style; textLength pins them to the cell grid
        row = scr.buffer[y]; x = 0
        while x < COLS:
            ch = row[x]
            if not ch.data.strip():
                x += 1; continue
            x2 = x; s = ch.data
            while x2 + 1 < COLS and _sty(row[x2 + 1]) == _sty(ch):
                x2 += 1; s += row[x2].data or ' '
            fg = color(ch.bg if ch.reverse else ch.fg, DEF_BG if ch.reverse else DEF_FG)
            a = f' fill="{fg}"' + (' font-weight="bold"' if ch.bold else '') + (' font-style="italic"' if ch.italics else '') + (' text-decoration="underline"' if ch.underscore else '')
            o.append(f'<text x="{x*CW:.1f}" y="{y*CH+13.5}" textLength="{(x2-x+1)*CW:.1f}" lengthAdjust="spacing" xml:space="preserve"{a}>{html.escape(s)}</text>')
            x = x2 + 1
    o.append('</g></svg>')
    open(path, 'w').write('\n'.join(o))
    if shutil.which('rsvg-convert'):
        subprocess.run(['rsvg-convert', '-z', '2', path, '-o', path[:-4] + '.png'], check=True)


def scrub(t, fake=False):
    """Replace the login with `you`, blank the unread badge/spinner; abort if anything private is visible.
    With `fake` the data is synthetic: the badge stays, and "rate limit" is just text in a PR title."""
    m = re.search(r'user: (\S+)', t.text())
    if m:
        login = m.group(1)
        for y in range(ROWS):
            line, i = t.scr.display[y], t.scr.display[y].find(login)
            while i >= 0:
                if line[i + len(login):i + len(login) + 1] != '/':  # keep `owner/repo` when the owner is the login
                    for k, c in enumerate('you'.ljust(len(login))):
                        t.scr.buffer[y][i + k] = t.scr.buffer[y][i + k]._replace(data=c)
                i = line.find(login, i + 1)
    hdr = t.scr.buffer[0]  # the badge is the user's real unread count
    line = ''.join(hdr[x].data or ' ' for x in range(COLS))
    for mm in ([] if fake else re.finditer(r'✉\s*\d+|[⠀-⣿]', line)):
        for x in range(mm.start(), mm.end()):
            hdr[x] = hdr[x]._replace(data=' ')
    low = t.text().lower()
    if 'rate limit' in low and not fake: raise RateLimited()
    for s in SECRETS:
        if s.lower() in low: raise SystemExit(f'private string {s!r} on screen')


class RateLimited(Exception): pass


def launch(repo, tmp):
    cfg = os.path.join(tmp, 'config'); os.makedirs(cfg)
    os.symlink(os.path.expanduser('~/.config/gh'), os.path.join(cfg, 'gh'))  # keep gh auth, nothing else
    env = {'XDG_CONFIG_HOME': cfg, 'XDG_STATE_HOME': os.path.join(tmp, 'state'), 'XDG_CACHE_HOME': os.path.join(tmp, 'cache')}
    for attempt in range(8):  # gh occasionally hits API rate limits; back off and retry
        t = T(['-R', repo, '--theme', 'dark'], env, cwd=tmp)  # non-git cwd: no local branch name in the header
        try:
            t.wait_for(r'\[1\] (PRs|Pull requests)', 15); return t
        except SystemExit:
            t.close(); time.sleep(30)
    raise SystemExit('gh-pulse never started (rate limited?)')


def pr(t, nums, tab=3):
    """Go to the `tab`-th list tab of panel 1 (2 = All open, 3 = Merged) and select the first PR of `nums` that is listed;
    falls back to the top row. Panel 1 is already focused at startup, so its number is not pressed."""
    for k in '345':  # warm panels 3-5 so every panel shows real rows and counts
        t.send(k, 1.5)
        try: t.wait_for(r'\[%s\][^\n]*\d' % k, 12)
        except SystemExit: pass  # the 80x24 titles carry no counts
        t.pump(1)
    t.send('1', 1)
    t.send('}' * tab, 1)
    for _ in range(30):
        if re.search(r'│#\d+ ', t.text()): break
        t.pump(1)
    t.pump(1)
    for n in nums:
        if re.search(rf'│#{n} ', t.text()): break
    else: n = None
    for _ in range(120 if n else 0):
        if f'#{n} ' in ''.join(l[53:] for l in t.scr.display[1:5]): break
        t.send('j', .25)
    t.pump(3)


def ctx(t, n=0):
    """Drill into the PR (Files/Commits/Checks/Comments), go down n files, open the diff."""
    t.send('\r', 2)
    t.send('j' * n, .5)
    t.send('\r', 1.5)


def set_layout(t, want):  # cycle `t` until the diff header shows the wanted layout
    for _ in range(4):
        if want in ''.join(t.scr.display[:5]): break
        t.send('t', .6)


def s_main(t):
    pr(t, [5])
def s_diff_split(t):
    pr(t, [4]); ctx(t, 0); t.send('f', .8); set_layout(t, 'auto:split')
def s_diff_prose(t):
    pr(t, [5]); ctx(t, 4); t.send('f', .8); set_layout(t, 'unified')
def s_files(t):
    pr(t, [5]); ctx(t, 5)
def s_comments(t):
    pr(t, [14583], 2); t.send('\r', 2); t.send(']' * 3, 2); t.pump(3)
def s_checks(t):
    pr(t, [5]); t.send('\r', 2); t.send(']]', 2); t.pump(3)
def s_actions(t):
    pr(t, [5]); t.send('4', 2); t.pump(3)
def s_releases(t):
    pr(t, [5]); t.send('5', 1.5); t.send('}}', 2); t.send('\r', 2); t.pump(2)
def s_menu(t):
    pr(t, [5]); t.send('x', 1)
def s_confirm(t):  # picks Approve and stops at the confirm popup; 'y' is never sent
    pr(t, [5]); t.send('x', 1); t.send('j', .5); t.send('\r', 1); t.send('\x13', 1)  # Ctrl-S on the comment popup opens the confirm
def s_help(t):
    pr(t, [5]); t.send('?', 1)
def s_compact(t):
    pr(t, [])  # the narrow layout moves the detail pane, so the row search in pr() would never match


GP, CLI = 'navbytes/gh-pulse', 'cli/cli'
SHOTS = {  # name -> (repo, steps, (cols, rows))
    '1-main': (GP, s_main, (140, 40)),
    '2-diff-split': (GP, s_diff_split, (140, 40)),
    '3-diff-prose': (GP, s_diff_prose, (140, 40)),
    '4-files': (GP, s_files, (140, 40)),
    '5-comments': (CLI, s_comments, (140, 40)),
    '6-actions': (GP, s_actions, (140, 40)),
    '6b-checks': (GP, s_checks, (140, 40)),
    '7-releases': (GP, s_releases, (140, 40)),
    '8-menu': (GP, s_menu, (140, 40)),
    '8b-confirm': (GP, s_confirm, (140, 40)),
    '9-help': (GP, s_help, (140, 40)),
    '10-compact': (GP, s_compact, (80, 24)),
}

FAKE = os.path.join(ROOT, 'scripts/fake-gh')


def launch_fake(tmp):
    """Start gh-pulse from a non-git cwd with a hermetic environment: fake `gh` first on PATH, empty gh config,
    and a config/state seeded with synthetic favorites, hidden repos and recent repos."""
    fx = os.path.join(FAKE, 'fixtures')
    cfg, state, cwd = (os.path.join(tmp, d) for d in ('config', 'state', 'cwd'))
    for d in ('config/gh-pulse', 'state/gh-pulse', 'gh', 'cwd', 'log'): os.makedirs(os.path.join(tmp, d))
    shutil.copy(os.path.join(fx, 'config.toml'), os.path.join(cfg, 'gh-pulse/config.toml'))
    shutil.copy(os.path.join(fx, 'recent.json'), os.path.join(state, 'gh-pulse/recent.json'))
    os.chmod(os.path.join(state, 'gh-pulse'), 0o700)
    open(os.path.join(tmp, 'gh/hosts.yml'), 'w').write('github.com:\n    user: you\n')  # what gh-pulse keys its disk cache by; no token
    env = {'PATH': f'{FAKE}:/usr/bin:/bin', 'HOME': tmp, 'LANG': 'en_US.UTF-8', 'XDG_CONFIG_HOME': cfg, 'XDG_STATE_HOME': state,
           'XDG_CACHE_HOME': os.path.join(tmp, 'cache'), 'GH_CONFIG_DIR': os.path.join(tmp, 'gh'), 'FAKE_GH_LOG': os.path.join(tmp, 'log')}
    for warm in (True, False):  # the first run only opens the repo browser, which fills the on-disk repo list the Repos panel reads
        t = T(['--theme', 'dark'], env, cwd=cwd, clean=True)
        t.wait_for(r'octo-org/api#482', 20)
        if not warm: return t
        t.send('B', 2); t.wait_for(r'Repositories \d+ of', 20); t.close()


def g_home(t): t.send('2', 2); t.send('3', 2); t.send('1', 2)  # sections search when first focused; load all, end on Review requested
def g_diff(t):  # first review request, Diff tab zoomed, split layout
    t.send(']]]', 2); t.send('f', .8); set_layout(t, 'auto:split')
def g_scope(t): g_home(t); t.send('s', 1)
def g_repos(t): g_home(t); t.send('4', 2); t.send('j', .8)
def g_browser(t): t.send('B', 3); t.send('\r', .8); t.send('.', 1)  # Enter leaves the search box; `.` shows the hidden repos
def g_inbox(t): t.send('N', 3)


FAKE_SHOTS = {  # name -> (steps, (cols, rows)); synthetic data from scripts/fake-gh
    '11-global-home': (g_home, (140, 40)),
    '12-global-diff': (g_diff, (140, 40)),
    '13-scope-picker': (g_scope, (140, 40)),
    '14-repos-panel': (g_repos, (140, 40)),
    '15-repo-browser': (g_browser, (140, 40)),
    '16-inbox': (g_inbox, (140, 40)),
    '17-global-compact': (g_home, (80, 24)),
}


def shoot(name, launch_fn, steps, title, fake):
    for attempt in range(6):
        with tempfile.TemporaryDirectory() as tmp:
            t = launch_fn(tmp)
            try:
                t.pump(3); steps(t); t.pump(1)
                scrub(t, fake)
                render_svg(t, os.path.join(OUT, name + '.svg'), title)
                print(f'== {name}'); print('\n'.join(l.rstrip() for l in t.scr.display))
                return
            except RateLimited:
                print(f'{name}: rate limited, waiting', file=sys.stderr); time.sleep(120)
            finally:
                t.close()
    raise SystemExit(f'{name}: still rate limited')


if __name__ == '__main__':
    os.makedirs(OUT, exist_ok=True)
    args = [a for a in sys.argv[1:] if a != '--fake']
    if '--fake' in sys.argv:
        for name in args or FAKE_SHOTS:
            steps, (COLS, ROWS) = FAKE_SHOTS[name]
            shoot(name, launch_fake, steps, 'gh-pulse · global home (synthetic data)', True)
    else:
        for name in args or SHOTS:
            repo, steps, (COLS, ROWS) = SHOTS[name]
            shoot(name, lambda tmp: launch(repo, tmp), steps, f'gh-pulse · {repo}', False)
