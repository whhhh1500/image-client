// Package store owns the SQLite database: schema migrations, the restricted
// SQL channel the frontend uses for its own tables, and asset history.
package store

import (
	"context"
	"fmt"
	"log/slog"
	"os"
	"path/filepath"
	"time"

	"github.com/google/uuid"
	"zombiezen.com/go/sqlite"
	"zombiezen.com/go/sqlite/sqlitex"

	"imageclient/internal/platform"
)

// DB is the application database. Connections come from a small WAL pool, so
// readers do not queue behind a long write.
type DB struct {
	pool *sqlitex.Pool
	path string
	// SessionID identifies this process run; work recorded by an earlier run
	// can be told apart from work still in flight.
	SessionID string
}

const busyTimeout = 5 * time.Second

func prepareConn(conn *sqlite.Conn) error {
	conn.SetBusyTimeout(busyTimeout)
	return sqlitex.ExecuteTransient(conn, "PRAGMA foreign_keys = ON", nil)
}

// Open opens (creating if needed) the database at path and migrates it to the
// current schema, taking a verified backup first when an existing database
// is upgraded.
func Open(path string) (*DB, error) {
	_, statErr := os.Stat(path)
	existed := statErr == nil
	if err := platform.MkdirPrivate(filepath.Dir(path)); err != nil {
		return nil, fmt.Errorf("创建数据库目录失败：%w", err)
	}

	// Migrate on a dedicated connection before the pool hands out any.
	conn, err := sqlite.OpenConn(path, sqlite.OpenReadWrite|sqlite.OpenCreate|sqlite.OpenWAL)
	if err != nil {
		return nil, fmt.Errorf("打开 SQLite 失败：%w", err)
	}
	// Each step runs only if the previous one succeeded; the first error wins.
	err = prepareConn(conn)
	if err == nil {
		err = sqlitex.ExecuteTransient(conn, "PRAGMA journal_mode = WAL", nil)
	}
	if err == nil {
		err = migrateWithBackup(conn, path, existed)
	}
	if closeErr := conn.Close(); err == nil && closeErr != nil {
		err = closeErr
	}
	if err != nil {
		return nil, fmt.Errorf("执行 SQLite 迁移失败：%w", err)
	}
	platform.SecureFile(path)

	pool, err := sqlitex.NewPool(path, sqlitex.PoolOptions{PoolSize: 4, PrepareConn: prepareConn})
	if err != nil {
		return nil, fmt.Errorf("打开 SQLite 连接池失败：%w", err)
	}
	slog.Info("database.backend_open", "path", path, "driver", "zombiezen", "journalMode", "WAL")
	return &DB{pool: pool, path: path, SessionID: uuid.NewString()}, nil
}

// Close releases every connection.
func (db *DB) Close() error { return db.pool.Close() }

// Path is the database file.
func (db *DB) Path() string { return db.path }

// With runs fn on a pooled connection.
func (db *DB) With(ctx context.Context, fn func(*sqlite.Conn) error) error {
	conn, err := db.pool.Take(ctx)
	if err != nil {
		return fmt.Errorf("获取数据库连接失败：%w", err)
	}
	defer db.pool.Put(conn)
	return fn(conn)
}

// Tx runs fn inside an IMMEDIATE transaction, committing when fn returns nil.
// Taking the write lock up front avoids the deadlock-prone upgrade from a
// read transaction.
func (db *DB) Tx(ctx context.Context, fn func(*sqlite.Conn) error) error {
	return db.With(ctx, func(conn *sqlite.Conn) (err error) {
		end, err := sqlitex.ImmediateTransaction(conn)
		if err != nil {
			return fmt.Errorf("开始数据库事务失败：%w", err)
		}
		defer end(&err)
		return fn(conn)
	})
}
