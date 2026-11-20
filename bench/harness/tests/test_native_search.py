"""Native search contracts shared by the baseline and all experiment arms."""
import os
from pathlib import Path

import pytest

from bench.harness.framework.executors import BaselineExecutor
from bench.harness.experiments.tools import ExperimentTools


@pytest.fixture
def search(tmp_path):
    (tmp_path / "src" / "nested").mkdir(parents=True)
    (tmp_path / "src" / "a.rs").write_text("// header\nfn target() {}\nfn other() {}\n")
    (tmp_path / "src" / "nested" / "b.rs").write_text("fn target_child() {}\n")
    executor = BaselineExecutor()
    executor.startup(str(tmp_path))
    return executor


@pytest.mark.parametrize("path", ["src/a.rs", "./src/a.rs"])
def test_source_file_matches_with_repository_path_and_line(search, path):
    result = search.execute("grep_search", {"pattern": "fn target", "path": path})
    assert result.content == "src/a.rs:2:fn target() {}"
    assert result.byte_size == len(result.content.encode())
    assert search.total_response_bytes() == result.byte_size


@pytest.mark.parametrize("path", [None, "", ".", "src"])
def test_directory_search_remains_recursive(search, path):
    args = {"pattern": "fn target"}
    if path is not None:
        args["path"] = path
    assert search.execute("grep_search", args).content.splitlines() == [
        "src/a.rs:2:fn target() {}", "src/nested/b.rs:1:fn target_child() {}"]


@pytest.mark.parametrize("path", ["src/a.rs", "src"])
def test_valid_no_matches_is_not_an_error(search, path):
    assert search.execute("grep_search", {"pattern": "no_such_symbol", "path": path}).content == "No matches found."


def test_absolute_source_path_and_invalid_regex(search, tmp_path):
    assert search.execute("grep_search", {"pattern": "target", "path": str(tmp_path / "src/a.rs")}).content == "src/a.rs:2:fn target() {}"
    assert search.execute("grep_search", {"pattern": "[", "path": "src/a.rs"}).content.startswith("Error:")


@pytest.mark.parametrize("path", ["missing.rs", "missing/dir", "src/a.rs/child", "../outside"])
def test_invalid_paths_are_errors_instead_of_empty_results(search, path):
    assert search.execute("grep_search", {"pattern": "target", "path": path}).content.startswith("Error:")


def test_unsupported_path_does_not_block(search, tmp_path):
    os.mkfifo(tmp_path / "pipe")
    assert "regular file or directory" in search.execute("grep_search", {"pattern": "target", "path": "pipe"}).content


def test_explicit_file_read_error_is_not_no_match(search, monkeypatch):
    def denied(*args, **kwargs):
        raise PermissionError("test denied")
    monkeypatch.setattr(Path, "read_text", denied)
    assert search.execute("grep_search", {"pattern": "target", "path": "src/a.rs"}).content.startswith("Error:")


def test_existing_size_and_match_limits(search, tmp_path):
    large = tmp_path / "src" / "large.rs"
    large.write_text("x" * (512 * 1024 + 1))
    assert "size limit" in search.execute("grep_search", {"pattern": "x", "path": "src/large.rs"}).content
    assert search.execute("grep_search", {"pattern": "x", "path": "src"}).content == "No matches found."
    (tmp_path / "src" / "many.rs").write_text("target\n" * 201)
    for path in ("src/many.rs", "src"):
        lines = search.execute("grep_search", {"pattern": "target", "path": path}).content.splitlines()
        assert len(lines) == 201
        assert lines[-1] == "[truncated: more than 200 matches]"


@pytest.mark.parametrize("arm", ["A", "B", "C"])
def test_experiment_arms_share_native_search_and_error_accounting(tmp_path, arm):
    class MCP:
        def get_tool_definitions(self):
            return []
    (tmp_path / "source.rs").write_text("fn real_symbol() {}")
    tools = ExperimentTools(tmp_path, MCP() if arm != "A" else None)
    assert tools.execute("grep_search", {"pattern": "real_symbol", "path": "source.rs"}).content == "source.rs:1:fn real_symbol() {}"
    assert tools.events[-1]["error"] is None
    tools.execute("grep_search", {"pattern": "absent", "path": "source.rs"})
    assert tools.events[-1]["error"] is None
    tools.execute("grep_search", {"pattern": "real_symbol", "path": "missing.rs"})
    assert tools.events[-1]["error"] == "native_tool_error"
