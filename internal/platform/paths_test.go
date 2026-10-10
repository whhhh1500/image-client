package platform

import (
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
)

func TestValidateOverride(t *testing.T) {
	tmp := t.TempDir()
	existingDir := filepath.Join(tmp, "data")
	if err := os.Mkdir(existingDir, 0o755); err != nil {
		t.Fatal(err)
	}
	existingFile := filepath.Join(tmp, "file.txt")
	if err := os.WriteFile(existingFile, []byte("x"), 0o600); err != nil {
		t.Fatal(err)
	}
	root := filepath.VolumeName(tmp) + string(filepath.Separator)
	absMissing := filepath.Join(tmp, "not", "yet")

	cases := []struct {
		name    string
		value   string
		wantErr bool
		want    string
	}{
		{"empty", "", true, ""},
		{"whitespace", "   \t", true, ""},
		{"relative", "relative/dir", true, ""},
		{"dot dot unix segment", tmp + "/a/../b", true, ""},
		{"dot dot backslash segment", tmp + `\..\b`, true, ""},
		{"filesystem root", root, true, ""},
		{"existing file", existingFile, true, ""},
		{"absolute missing directory", absMissing, false, absMissing},
		{"existing directory", existingDir, false, existingDir},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got, err := validateOverride(tc.value)
			if tc.wantErr {
				if err == nil {
					t.Fatalf("validateOverride(%q) = %q, want error", tc.value, got)
				}
				if !strings.Contains(err.Error(), DataDirEnv) {
					t.Fatalf("error %q does not name %s", err, DataDirEnv)
				}
				return
			}
			if err != nil {
				t.Fatalf("validateOverride(%q) error: %v", tc.value, err)
			}
			if got != tc.want {
				t.Fatalf("validateOverride(%q) = %q, want %q", tc.value, got, tc.want)
			}
		})
	}
}

func TestDataRootDefaultsToHomeImageClient(t *testing.T) {
	home := t.TempDir()
	first, second := "HOME", "USERPROFILE"
	if runtime.GOOS == "windows" {
		first, second = "USERPROFILE", "HOME"
	}
	env := func(values map[string]string) func(string) (string, bool) {
		return func(name string) (string, bool) {
			v, ok := values[name]
			return v, ok
		}
	}

	cases := []struct {
		name   string
		values map[string]string
		want   string
	}{
		{"native variable preferred", map[string]string{first: home, second: "/other"}, filepath.Join(home, "ImageClient")},
		{"falls back to the other variable", map[string]string{second: home}, filepath.Join(home, "ImageClient")},
		{"empty native variable is skipped", map[string]string{first: "", second: home}, filepath.Join(home, "ImageClient")},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got, err := dataRoot("", env(tc.values))
			if err != nil {
				t.Fatalf("dataRoot: %v", err)
			}
			if got != tc.want {
				t.Fatalf("dataRoot = %q, want %q", got, tc.want)
			}
		})
	}
}

func TestDataRootUsesValidOverride(t *testing.T) {
	override := filepath.Join(t.TempDir(), "isolated")
	lookup := func(name string) (string, bool) {
		if name == DataDirEnv {
			return override, true
		}
		return "", false
	}
	got, err := dataRoot(override, lookup)
	if err != nil || got != override {
		t.Fatalf("dataRoot override = %q, %v; want %q", got, err, override)
	}

	_, err = dataRoot("relative", lookup)
	if err == nil {
		t.Fatal("dataRoot with an invalid override must fail rather than fall back")
	}
}

func TestPathsLayout(t *testing.T) {
	root := filepath.Join("base", "root")
	p := Paths{Root: root}
	cases := map[string][2]string{
		"assets":   {p.Assets(), filepath.Join(root, "assets")},
		"logs":     {p.Logs(), filepath.Join(root, "logs")},
		"prompt":   {p.PromptlibImages(), filepath.Join(root, "promptlib-images")},
		"database": {p.Database(), filepath.Join(root, "image-client.db")},
		"config":   {p.BackendConfig(), filepath.Join(root, "backend-config.json")},
	}
	for name, pair := range cases {
		if pair[0] != pair[1] {
			t.Errorf("%s = %q, want %q", name, pair[0], pair[1])
		}
	}
	if want := strings.ReplaceAll(root, `\`, "/"); p.Slash() != want {
		t.Errorf("Slash() = %q, want %q", p.Slash(), want)
	}
	if strings.Contains(p.Slash(), `\`) {
		t.Errorf("Slash() must not contain backslashes: %q", p.Slash())
	}
}

func TestPathsEnsureCreatesFixedDirectories(t *testing.T) {
	p := Paths{Root: filepath.Join(t.TempDir(), "data")}
	if err := p.Ensure(); err != nil {
		t.Fatalf("Ensure: %v", err)
	}
	for _, dir := range []string{p.Root, p.Assets(), p.Logs(), p.PromptlibImages()} {
		info, err := os.Stat(dir)
		if err != nil || !info.IsDir() {
			t.Fatalf("Ensure did not create %s: %v", dir, err)
		}
	}
	if err := p.Ensure(); err != nil {
		t.Fatalf("Ensure must be idempotent: %v", err)
	}
}
