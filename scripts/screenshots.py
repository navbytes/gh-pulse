#!/usr/bin/env python3
"""Drive the real gh-pulse binary in a pty (pyte) against public repos and render docs/img/*.svg|png.

Usage: scripts/screenshots.py [shot ...]   (needs `pip install pyte`, `cargo build --release`;
PNG conversion uses rsvg-convert if present). Read-only: never confirms an action.
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
    def __init__(s, args, env):
        s.scr = pyte.Screen(COLS, ROWS); s.st = pyte.ByteStream(s.scr)
        e = dict(os.environ, TERM='xterm-256color', COLORTERM='truecolor', **env)
        s.pid, s.fd = pty.fork()
        if s.pid == 0:
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


def scrub(t):
    """Replace the login in the header with `you`; abort if anything private is visible."""
    m = re.search(r'user: (\S+)', t.text())
    if m:
        login = m.group(1)
        for y in range(ROWS):
            i = t.scr.display[y].find(login)
            while i >= 0:
                for k, c in enumerate('you'.ljust(len(login))):
                    t.scr.buffer[y][i + k] = t.scr.buffer[y][i + k]._replace(data=c)
                i = t.scr.display[y].find(login, i + 1)
    low = t.text().lower()
    for s in SECRETS:
        if s.lower() in low: raise SystemExit(f'private string {s!r} on screen')


def launch(repo, tmp):
    cfg = os.path.join(tmp, 'config'); os.makedirs(cfg)
    os.symlink(os.path.expanduser('~/.config/gh'), os.path.join(cfg, 'gh'))  # keep gh auth, nothing else
    env = {'XDG_CONFIG_HOME': cfg, 'XDG_STATE_HOME': os.path.join(tmp, 'state')}
    for attempt in range(8):  # gh occasionally hits API rate limits; back off and retry
        t = T(['-R', repo, '--theme', 'dark'], env)
        try:
            t.wait_for(r'Status', 8); return t
        except SystemExit:
            t.close(); time.sleep(30)
    raise SystemExit('gh-pulse never started (rate limited?)')


def pr(t, sub):
    """Focus the PR list on 'All open' and move to the PR whose title contains `sub`."""
    t.send('2', .4); t.send('}', .4); t.send('}', 1)
    for _ in range(120):
        if sub in t.scr.display[2][53:]: break
        t.send('j', .25)
    else: raise SystemExit(f'PR {sub!r} not found (is it still open?)')
    t.pump(3)


def ctx(t, n=0):
    """Drill into the PR (Files/Commits/Checks/Comments), go down n files, open the diff."""
    t.send('\r', 2)
    t.send('j' * n, .5)
    t.send('\r', 1.5)


def s_main(t):
    pr(t, 'drain pending TTY')
def s_diff_split(t):
    pr(t, 'terminal hyperlinks'); ctx(t, 9); t.send('f', .8)
    for _ in range(3):
        if 'auto:split' in t.scr.display[2]: break
        t.send('t', .6)
def s_diff_prose(t):
    pr(t, 'Add ACCESSIBILITY.md'); ctx(t, 0); t.send('f', .8)
    for _ in range(4):
        if 'unified' in t.scr.display[2] and 'auto' not in t.scr.display[2]: break
        t.send('t', .6)
def s_files(t):
    pr(t, 'terminal hyperlinks'); ctx(t, 3)
def s_comments(t):
    pr(t, 'terminal hyperlinks'); t.send(']', .6); t.send(']', 2)
def s_actions(t):
    t.send('5', 2)
def s_menu(t):
    pr(t, 'drain pending TTY'); t.send('x', 1)
def s_confirm(t):  # picks "Close PR" and stops at the confirm popup; 'y' is never sent
    pr(t, 'drain pending TTY'); t.send('x', 1); t.send('j' * 6, .5); t.send('\r', 1)
def s_help(t):
    t.send('?', 1)


SHOTS = {  # name -> (repo, steps)
    '1-main': ('charmbracelet/bubbletea', s_main),
    '2-diff-split': ('cli/cli', s_diff_split),
    '3-diff-prose': ('cli/cli', s_diff_prose),
    '4-files': ('cli/cli', s_files),
    '5-comments': ('cli/cli', s_comments),
    '6-actions': ('charmbracelet/bubbletea', s_actions),
    '7-menu': ('charmbracelet/bubbletea', s_menu),
    '7b-confirm': ('charmbracelet/bubbletea', s_confirm),
    '8-help': ('charmbracelet/bubbletea', s_help),
}

if __name__ == '__main__':
    os.makedirs(OUT, exist_ok=True)
    for name in sys.argv[1:] or SHOTS:
        repo, steps = SHOTS[name]
        with tempfile.TemporaryDirectory() as tmp:
            t = launch(repo, tmp)
            try:
                t.pump(3); steps(t); t.pump(1)
                scrub(t)
                render_svg(t, os.path.join(OUT, name + '.svg'), f'gh-pulse · {repo}')
                print(f'== {name}'); print('\n'.join(l.rstrip() for l in t.scr.display))
            finally:
                t.close()
