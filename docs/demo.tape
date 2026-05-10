# pgtop demo recording — produces docs/demo.gif
# Run with:  vhs docs/demo.tape
# Prerequisite: a Postgres reachable via DATABASE_URL or the local docker-compose.

Output docs/demo.gif

Set FontSize 14
Set Width 1200
Set Height 700
Set TypingSpeed 60ms
Set PlaybackSpeed 1.0
Set Theme "Dracula"

# Boot pgtop against the local docker-compose Postgres. Adjust the DSN if your
# setup differs.
Type "pgtop --dsn 'host=localhost user=pgtop password=pgtop port=5433 dbname=pgtop'"
Enter
Sleep 3s

# --- Activity tab: filter + sort -------------------------------------------
# Filter by query substring.
Type "/"
Type "select"
Enter
Sleep 2s

# Clear filter and cycle sort columns.
Escape
Sleep 500ms
Type "s"
Sleep 1s
Type "s"
Sleep 1s
Type "S"
Sleep 1s

# Open detail popup for the highlighted backend.
Enter
Sleep 2s
Escape
Sleep 500ms

# --- Switch tabs -----------------------------------------------------------
Type "2"
Sleep 2s
Type "3"
Sleep 2s
Type "4"
Sleep 2s

# Back to activity, quit.
Type "1"
Sleep 1500ms
Type "q"
Sleep 1s
