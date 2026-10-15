from __future__ import annotations

import argparse
import ast
import json
import re
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
DEFAULT_INPUT = ROOT / "packages" / "cloud-api" / "openapi.json"
OUTPUT = ROOT / "clients" / "python" / "simforge_cloud" / "models"
SHARED_MODULE = "common"
UNROUTED_MODULE = "unrouted"
MAX_LINES = 1000
LINE_LENGTH = 88
SCHEMA_REF = "#/components/schemas/"
HTTP_METHODS = {"get", "put", "post", "delete", "options", "head", "patch", "trace"}


def run_codegen(input_path: Path) -> str:
    with tempfile.TemporaryDirectory(prefix="simforge-cloud-codegen-") as directory:
        output_path = Path(directory) / "models.py"
        command = [
            "datamodel-codegen",
            "--input",
            str(input_path),
            "--input-file-type",
            "openapi",
            "--output",
            str(output_path),
            "--output-model-type",
            "pydantic_v2.BaseModel",
            "--target-python-version",
            "3.10",
            "--snake-case-field",
            "--enum-field-as-literal",
            "all",
            "--use-standard-collections",
            "--disable-timestamp",
        ]
        subprocess.run(command, check=True)
        return output_path.read_text()


def collect_schema_refs(node: object, document: dict, seen: set[str], refs: set[str]) -> None:
    if isinstance(node, dict):
        ref = node.get("$ref")
        if isinstance(ref, str):
            if ref.startswith(SCHEMA_REF):
                refs.add(ref[len(SCHEMA_REF) :])
            elif ref.startswith("#/") and ref not in seen:
                seen.add(ref)
                target: object = document
                for part in ref[2:].split("/"):
                    target = target[part.replace("~1", "/").replace("~0", "~")]
                collect_schema_refs(target, document, seen, refs)
        for value in node.values():
            collect_schema_refs(value, document, seen, refs)
    elif isinstance(node, list):
        for value in node:
            collect_schema_refs(value, document, seen, refs)


def path_domain(path: str) -> str:
    segments = [segment for segment in path.strip("/").split("/") if segment]
    while segments and (segments[0] == "api" or re.fullmatch(r"v\d+", segments[0])):
        segments.pop(0)
    if not segments or segments[0].startswith("{"):
        raise SystemExit(f"OpenAPI path has no resource domain: {path}")
    domain = re.sub(r"\W", "_", segments[0].replace("-", "_")).lower()
    if domain in {SHARED_MODULE, UNROUTED_MODULE} or not domain.isidentifier():
        raise SystemExit(f"OpenAPI path domain {domain!r} is not a usable module name: {path}")
    return domain


def operation_seeds(document: dict) -> dict[str, set[str]]:
    seeds: dict[str, set[str]] = {}
    for path, item in document.get("paths", {}).items():
        refs: set[str] = set()
        shared = {key: value for key, value in item.items() if key not in HTTP_METHODS}
        collect_schema_refs(shared, document, set(), refs)
        for method, operation in item.items():
            if method in HTTP_METHODS:
                collect_schema_refs(operation, document, set(), refs)
        domain = path_domain(path)
        for name in refs:
            seeds.setdefault(name, set()).add(domain)
    return seeds


def statement_names(node: ast.stmt) -> list[str]:
    if isinstance(node, ast.ClassDef):
        return [node.name]
    if isinstance(node, ast.AnnAssign) and isinstance(node.target, ast.Name):
        return [node.target.id]
    if isinstance(node, ast.Assign) and all(isinstance(target, ast.Name) for target in node.targets):
        return [target.id for target in node.targets]
    raise SystemExit(f"Unsupported generated statement at line {node.lineno}: {ast.dump(node)[:120]}")


def used_names(node: ast.AST) -> set[str]:
    return {child.id for child in ast.walk(node) if isinstance(child, ast.Name)}


def render_import(module: str, names: list[str]) -> str:
    single = f"from {module} import {', '.join(names)}"
    if len(single) <= LINE_LENGTH:
        return single
    body = "".join(f"    {name},\n" for name in names)
    return f"from {module} import (\n{body})"


def render_module(header: str, imports: list[str], local_imports: list[str], body: str, gap: str) -> str:
    sections = [header.rstrip("\n"), *imports, "\n".join(local_imports)]
    return "\n\n".join(section for section in sections if section) + gap + body + "\n"


def segment(lines: list[str], node: ast.stmt) -> str:
    return "".join(lines[node.lineno - 1 : node.end_lineno]).rstrip("\n")


def split_models(source: str, document: dict) -> dict[str, str]:
    tree = ast.parse(source)
    lines = source.splitlines(keepends=True)
    header = "".join(lines[: tree.body[0].lineno - 1])
    future: list[str] = []
    external: list[tuple[str, list[str]]] = []
    definitions: list[tuple[list[str], ast.stmt]] = []
    for node in tree.body:
        if isinstance(node, ast.ImportFrom) and node.module == "__future__":
            future.extend(alias.name for alias in node.names)
        elif isinstance(node, ast.ImportFrom) and node.level == 0 and node.module:
            external.append((node.module, [alias.asname or alias.name for alias in node.names]))
        elif isinstance(node, (ast.Import, ast.ImportFrom)):
            raise SystemExit(f"Unsupported generated import at line {node.lineno}")
        else:
            definitions.append((statement_names(node), node))

    owner: dict[str, int] = {}
    for index, (names, _) in enumerate(definitions):
        for name in names:
            if name in owner:
                raise SystemExit(f"Generated model {name} is defined twice")
            owner[name] = index

    dependencies = [
        {owner[name] for name in used_names(node) if name in owner} - {index}
        for index, (_, node) in enumerate(definitions)
    ]
    for index, deps in enumerate(dependencies):
        forward = [definitions[dep][0][0] for dep in deps if dep > index]
        if forward:
            raise SystemExit(f"{definitions[index][0][0]} references later definitions {forward}")

    domains: list[set[str]] = [set() for _ in definitions]
    seeds = operation_seeds(document)
    components = document.get("components", {}).get("schemas", {})
    missing = sorted(name for name in components if name not in owner)
    if missing:
        raise SystemExit(f"OpenAPI schemas have no generated model of the same name: {missing}")
    for name, seed_domains in seeds.items():
        if name in components:
            domains[owner[name]] |= seed_domains
    for index in range(len(definitions) - 1, -1, -1):
        for dep in dependencies[index]:
            domains[dep] |= domains[index]

    def module_for(index: int) -> str:
        if not domains[index]:
            return UNROUTED_MODULE
        if len(domains[index]) > 1:
            return SHARED_MODULE
        return next(iter(domains[index]))

    modules: dict[str, list[int]] = {}
    for index in range(len(definitions)):
        modules.setdefault(module_for(index), []).append(index)

    rank = {SHARED_MODULE: 0, UNROUTED_MODULE: 2}
    ordered = sorted(modules, key=lambda module: (rank.get(module, 1), module))
    position = {module: order for order, module in enumerate(ordered)}

    files: dict[str, str] = {}
    exports: dict[str, list[str]] = {}
    future_import = [render_import("__future__", future)] if future else []
    for module in ordered:
        members = modules[module]
        local = {name for index in members for name in definitions[index][0]}
        referenced = set().union(*(used_names(definitions[index][1]) for index in members))
        imports = list(future_import)
        for external_module, names in external:
            needed = [name for name in names if name in referenced and name not in local]
            if needed:
                imports.append(render_import(external_module, needed))
        sibling: dict[str, set[str]] = {}
        for index in members:
            for dep in dependencies[index]:
                dep_module = module_for(dep)
                if dep_module != module:
                    if position[dep_module] > position[module]:
                        raise SystemExit(f"Module {module} would import later module {dep_module}")
                    sibling.setdefault(dep_module, set()).update(definitions[dep][0])
        local_imports = [render_import(f".{dep}", sorted(sibling[dep])) for dep in sorted(sibling)]
        blocks = "\n\n\n".join(segment(lines, definitions[index][1]) for index in members)
        files[f"{module}.py"] = render_module(header, imports, local_imports, blocks, "\n\n\n")
        exports[module] = [name for index in members for name in definitions[index][0]]

    init_imports = [render_import(f".{module}", sorted(exports[module])) for module in sorted(exports)]
    public = sorted(name for names in exports.values() for name in names)
    all_block = "__all__ = [\n" + "".join(f'    "{name}",\n' for name in public) + "]"
    files["__init__.py"] = render_module(header, future_import, init_imports, all_block, "\n\n")

    sizes = {name: text.count("\n") for name, text in files.items()}
    oversized = sorted(f"{name} ({count} lines)" for name, count in sizes.items() if count > MAX_LINES)
    if oversized:
        raise SystemExit(f"Generated model modules exceed {MAX_LINES} lines: {oversized}")
    return files


def generate(input_path: Path) -> dict[str, str]:
    document = json.loads(input_path.read_text())
    return split_models(run_codegen(input_path), document)


def existing_files() -> dict[str, str]:
    if not OUTPUT.is_dir():
        return {}
    return {path.name: path.read_text() for path in sorted(OUTPUT.glob("*.py"))}


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--input", type=Path, default=DEFAULT_INPUT)
    args = parser.parse_args()
    if not args.input.is_file():
        raise SystemExit(f"OpenAPI input does not exist: {args.input}")

    files = generate(args.input)
    if args.check:
        if existing_files() != files:
            print(f"Generated models are out of date; run {sys.argv[0]}", file=sys.stderr)
            return 1
        return 0

    OUTPUT.mkdir(parents=True, exist_ok=True)
    for stale in sorted(set(existing_files()) - set(files)):
        (OUTPUT / stale).unlink()
    for name, text in files.items():
        (OUTPUT / name).write_text(text)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
