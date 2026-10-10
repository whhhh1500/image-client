package app

import (
	"context"
	"fmt"

	"imageclient/internal/comic"
	"imageclient/internal/gen"
	"imageclient/internal/media"
	"imageclient/internal/novel"
	"imageclient/internal/restapi"
)

// wireServices constructs the domain services, registers their commands,
// runs the startup recovery that must finish before the window opens, and
// queues their shutdown.
func (a *App) wireServices() error {
	files := media.New(media.Deps{Paths: a.paths, Config: a.config, DB: a.db, ComicCatalog: a.mediaComicCatalog})
	files.Register(a.router)

	generator := gen.New(gen.Deps{Paths: a.paths, Config: a.config, Events: a, DB: a.db, ComicCatalog: a.genComicCatalog})
	generator.Register(a.router)

	comics := comic.New(comic.Deps{
		Paths:  a.paths,
		Config: a.config,
		DB:     a.db,
		Events: a,
		Text:   comicText{gen: generator},
		Images: comicImages{gen: generator},
	})
	// Jobs left running by a previous process are marked interrupted before
	// the frontend can see them; a failure here aborts startup, as before.
	if _, err := comics.Recover(context.Background()); err != nil {
		return fmt.Errorf("恢复漫画任务失败：%w", err)
	}
	comics.Register(a.router)
	a.onClose(comics.Close)

	novel.New(novel.Deps{DB: a.db}).Register(a.router)

	// The local REST API serves while the app runs; a taken port only logs.
	api := restapi.New(restapi.Deps{
		Config:  a.config,
		DB:      a.db,
		Feed:    a.feed,
		Events:  a,
		Paths:   a.paths,
		Version: a.version,
		Images:  restImages{gen: generator},
		Videos:  restVideos{gen: generator},
		Text:    restText{gen: generator, config: a.config},
		Catalog: restCatalog{gen: generator},
		Files:   restFiles{media: files},
	})
	if err := api.Start(context.Background()); err != nil {
		return fmt.Errorf("启动本地接口失败：%w", err)
	}
	// Registered last so it runs first: stop taking API work before the
	// services it calls shut down.
	a.onClose(api.Shutdown)
	return nil
}
