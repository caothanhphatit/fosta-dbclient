#!/usr/bin/env sh
export MSYS_NO_PATHCONV=1  # Git Bash on Windows would rewrite /opt/... paths
# Waits for the SQL Server container and loads dev/seed/mssql.sql (skips when the ERP database already exists).
set -e
cd "$(dirname "$0")"
SQLCMD="docker compose -f docker-compose.yml exec -T mssql /opt/mssql-tools18/bin/sqlcmd -C -S localhost -U sa -P Studio#Pass2026"
for attempt in $(seq 1 60); do
  if $SQLCMD -Q "SELECT 1" >/dev/null 2>&1; then break; fi
  sleep 2
done
existing=$($SQLCMD -h -1 -W -Q "SET NOCOUNT ON; SELECT COUNT(*) FROM sys.databases WHERE name = 'ERP'" | tr -d '[:space:]')
if [ "$existing" = "1" ]; then
  echo "ERP already exists; drop it to reseed"
  exit 0
fi
$SQLCMD -b -i /seed/mssql.sql
echo "ERP seeded"
