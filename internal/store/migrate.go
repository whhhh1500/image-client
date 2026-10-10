package store

import (
	"cmp"
	"embed"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"time"

	"github.com/google/uuid"
	"zombiezen.com/go/sqlite"
	"zombiezen.com/go/sqlite/sqlitex"

	"imageclient/internal/platform"
)

// Migrations are NNNN_name.sql; the number is the PRAGMA user_version the
// script brings the database to. The files are shared with earlier releases
// and must never be edited, only appended to.
//
//go:embed migrations/*.sql
var migrationFS embed.FS

type migration struct {
	version int
	name    string
	sql     string
}

func loadMigrations() ([]migration, error) {
	entries, err := fs.ReadDir(migrationFS, "migrations")
	if err != nil {
		return nil, err
	}
	var list []migration
	for _, entry := range entries {
		number, _, ok := strings.Cut(entry.Name(), "_")
		version, err := strconv.Atoi(number)
		if !ok || err != nil {
			return nil, fmt.Errorf("迁移文件名无效：%s", entry.Name())
		}
		data, err := migrationFS.ReadFile("migrations/" + entry.Name())
		if err != nil {
			return nil, err
		}
		list = append(list, migration{version: version, name: entry.Name(), sql: string(data)})
	}
	slices.SortFunc(list, func(a, b migration) int { return a.version - b.version })
	for i, m := range list {
		if m.version != i+1 {
			return nil, fmt.Errorf("迁移版本不连续：%s", m.name)
		}
	}
	return list, nil
}

// SchemaVersion is the newest schema this build understands.
func SchemaVersion() int {
	list, err := loadMigrations()
	if err != nil {
		return 0
	}
	return len(list)
}

func userVersion(conn *sqlite.Conn) (int, error) {
	var version int
	err := sqlitex.ExecuteTransient(conn, "PRAGMA user_version", &sqlitex.ExecOptions{
		ResultFunc: func(stmt *sqlite.Stmt) error { version = stmt.ColumnInt(0); return nil },
	})
	if err != nil {
		return 0, fmt.Errorf("读取 SQLite schema 版本失败：%w", err)
	}
	return version, nil
}

func migrateWithBackup(conn *sqlite.Conn, path string, existed bool) error {
	list, err := loadMigrations()
	if err != nil {
		return err
	}
	current, err := userVersion(conn)
	if err != nil {
		return err
	}
	latest := len(list)
	if current > latest {
		return fmt.Errorf("数据库 schema v%d 高于当前程序支持的 v%d，请升级应用", current, latest)
	}
	if existed && current < latest {
		if err := backupBeforeMigration(conn, path, current); err != nil {
			return err
		}
	}
	for _, m := range list[current:] {
		if err := apply(conn, m); err != nil {
			return err
		}
	}
	return nil
}

// tableRebuilds lists migrations that rebuild a table other tables reference.
// They run with foreign keys off (outside the transaction, as SQLite requires)
// and legacy ALTER semantics, and are validated before commit.
var tableRebuilds = map[int]bool{22: true}

func apply(conn *sqlite.Conn, m migration) (err error) {
	label := fmt.Sprintf("v%d", m.version)
	if tableRebuilds[m.version] {
		if err := sqlitex.ExecuteTransient(conn, "PRAGMA foreign_keys = OFF", nil); err != nil {
			return err
		}
		if err := sqlitex.ExecuteTransient(conn, "PRAGMA legacy_alter_table = ON", nil); err != nil {
			return err
		}
		defer func() {
			sqlitex.ExecuteTransient(conn, "PRAGMA legacy_alter_table = OFF", nil)
			sqlitex.ExecuteTransient(conn, "PRAGMA foreign_keys = ON", nil)
		}()
	}
	end, err := sqlitex.ImmediateTransaction(conn)
	if err != nil {
		return fmt.Errorf("开始 SQLite %s 迁移事务失败：%w", label, err)
	}
	defer end(&err)
	if err := sqlitex.ExecuteScript(conn, m.sql, nil); err != nil {
		return fmt.Errorf("执行 %s SQLite 迁移失败：%w", label, err)
	}
	if tableRebuilds[m.version] {
		violations := false
		err := sqlitex.ExecuteTransient(conn, "SELECT EXISTS(SELECT 1 FROM pragma_foreign_key_check)", &sqlitex.ExecOptions{
			ResultFunc: func(stmt *sqlite.Stmt) error { violations = stmt.ColumnBool(0); return nil },
		})
		if err != nil {
			return err
		}
		if violations {
			return fmt.Errorf("%s 外键校验失败", label)
		}
	}
	return sqlitex.ExecuteTransient(conn, fmt.Sprintf("PRAGMA user_version = %d", m.version), nil)
}

const (
	backupPagesPerStep = 100
	backupRetryDelay   = 25 * time.Millisecond
	// Only waiting for a lock counts against this; a large database on a slow
	// disk legitimately takes longer to copy.
	backupLockTimeout = 3 * time.Second
	backupsKept       = 3
)

// backupBeforeMigration copies the database with the online backup API to
// backups/<stem>-pre-migration-v<version>-<unixnano>-<uuid>.sqlite, verifies
// the copy, then keeps only the newest few.
func backupBeforeMigration(src *sqlite.Conn, path string, version int) error {
	dir := filepath.Join(filepath.Dir(path), "backups")
	if err := platform.MkdirPrivate(dir); err != nil {
		return fmt.Errorf("创建 SQLite 迁移前备份目录失败：%w", err)
	}
	prefix := strings.TrimSuffix(filepath.Base(path), filepath.Ext(path)) + "-pre-migration-v"
	final := filepath.Join(dir, fmt.Sprintf("%s%d-%d-%s.sqlite", prefix, version, time.Now().UnixNano(), uuid.NewString()))
	tmp := filepath.Join(dir, ".migration-"+uuid.NewString()+".tmp")

	err := func() error {
		dst, err := sqlite.OpenConn(tmp, sqlite.OpenReadWrite|sqlite.OpenCreate)
		if err != nil {
			return fmt.Errorf("创建 SQLite 迁移前备份临时文件失败：%w", err)
		}
		copyErr := copyDatabase(dst, src)
		if closeErr := dst.Close(); copyErr == nil && closeErr != nil {
			copyErr = fmt.Errorf("关闭 SQLite 迁移前备份临时文件失败：%w", closeErr)
		}
		if copyErr != nil {
			return copyErr
		}
		if err := verifyBackup(tmp, version); err != nil {
			return err
		}
		if err := syncFile(tmp); err != nil {
			return fmt.Errorf("同步 SQLite 迁移前备份临时文件失败：%w", err)
		}
		if err := os.Rename(tmp, final); err != nil {
			return fmt.Errorf("提交 SQLite 迁移前备份失败：%w", err)
		}
		platform.SecureFile(final)
		return pruneBackups(dir, prefix)
	}()
	if err != nil {
		os.Remove(tmp)
	}
	return err
}

func copyDatabase(dst, src *sqlite.Conn) error {
	backup, err := sqlite.NewBackup(dst, "main", src, "main")
	if err != nil {
		return fmt.Errorf("初始化 SQLite Online Backup 失败：%w", err)
	}
	defer backup.Close()
	deadline := time.Now().Add(backupLockTimeout)
	for {
		more, err := backup.Step(backupPagesPerStep)
		switch code := sqlite.ErrCode(err); {
		case err == nil && !more:
			return nil
		case err == nil:
			deadline = time.Now().Add(backupLockTimeout) // progress resets the lock wait
		case code == sqlite.ResultBusy || code == sqlite.ResultLocked:
			if time.Now().After(deadline) {
				return fmt.Errorf("SQLite 迁移前备份在 3 秒内未获得可用锁")
			}
			time.Sleep(backupRetryDelay)
		default:
			return fmt.Errorf("执行 SQLite Online Backup 失败：%w", err)
		}
	}
}

func verifyBackup(path string, version int) error {
	conn, err := sqlite.OpenConn(path, sqlite.OpenReadOnly)
	if err != nil {
		return fmt.Errorf("打开 SQLite 迁移前备份进行验证失败：%w", err)
	}
	defer conn.Close()
	integrity := ""
	err = sqlitex.ExecuteTransient(conn, "PRAGMA integrity_check", &sqlitex.ExecOptions{
		ResultFunc: func(stmt *sqlite.Stmt) error { integrity = stmt.ColumnText(0); return nil },
	})
	if err != nil || integrity != "ok" {
		return fmt.Errorf("SQLite 迁移前备份完整性校验未通过")
	}
	got, err := userVersion(conn)
	if err != nil {
		return err
	}
	if got != version {
		return fmt.Errorf("SQLite 迁移前备份 schema 版本与源数据库不一致")
	}
	return nil
}

func syncFile(path string) error {
	file, err := os.OpenFile(path, os.O_RDWR, 0)
	if err != nil {
		return err
	}
	defer file.Close()
	return file.Sync()
}

// pruneBackups keeps the newest backups by their embedded timestamp.
func pruneBackups(dir, prefix string) error {
	entries, err := os.ReadDir(dir)
	if err != nil {
		return fmt.Errorf("读取 SQLite 备份目录失败：%w", err)
	}
	type backupFile struct {
		name  string
		stamp int64
	}
	var files []backupFile
	for _, entry := range entries {
		name := entry.Name()
		if !entry.Type().IsRegular() || !strings.HasPrefix(name, prefix) || !strings.HasSuffix(name, ".sqlite") {
			continue
		}
		// <version>-<unixnano>-<uuid>
		parts := strings.SplitN(strings.TrimSuffix(strings.TrimPrefix(name, prefix), ".sqlite"), "-", 3)
		if len(parts) != 3 {
			continue
		}
		stamp, err := strconv.ParseInt(parts[1], 10, 64)
		if err != nil {
			continue
		}
		files = append(files, backupFile{name, stamp})
	}
	slices.SortFunc(files, func(a, b backupFile) int {
		return cmp.Or(cmp.Compare(b.stamp, a.stamp), strings.Compare(b.name, a.name))
	})
	for _, old := range files[min(len(files), backupsKept):] {
		if err := os.Remove(filepath.Join(dir, old.name)); err != nil {
			return fmt.Errorf("清理过期 SQLite 迁移前备份失败：%w", err)
		}
	}
	return nil
}
