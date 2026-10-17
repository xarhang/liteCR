import { readFileSync, readdirSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import ts from 'typescript';

const scriptDir = dirname(fileURLToPath(import.meta.url));
const repositoryRoot = resolve(scriptDir, '../../..');
const webSource = resolve(repositoryRoot, 'apps/web/src');
const catalogPath = resolve(repositoryRoot, 'packages/localization/src/webCatalogPreview.ts');

function sourceFiles(directory) {
  return readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) return sourceFiles(path);
    return /\.(tsx|ts)$/.test(entry.name) && !entry.name.endsWith('.test.ts') && !entry.name.endsWith('.test.tsx') ? [path] : [];
  });
}

function stringLiterals(node) {
  if (!node) return [];
  if (ts.isStringLiteral(node) || ts.isNoSubstitutionTemplateLiteral(node)) return [node.text];
  if (ts.isConditionalExpression(node)) return [...stringLiterals(node.whenTrue), ...stringLiterals(node.whenFalse)];
  return [];
}

function usedUiKeys(files) {
  const trees = files.map((path) => {
    const text = readFileSync(path, 'utf8');
    return ts.createSourceFile(path, text, ts.ScriptTarget.Latest, true, path.endsWith('.tsx') ? ts.ScriptKind.TSX : ts.ScriptKind.TS);
  });
  const keys = new Set();
  for (const tree of trees) {
    const helpers = new Map();
    const registerHelper = (name, parameters, body) => {
      const keyParameter = parameters[1]?.name;
      const returned = ts.isBlock(body) ? body.statements.find(ts.isReturnStatement)?.expression : body;
      if (keyParameter && ts.isIdentifier(keyParameter) && returned && ts.isCallExpression(returned)) {
        const keyExpr = returned.arguments[0];
        if (keyExpr && ts.isBinaryExpression(keyExpr) && keyExpr.operatorToken.kind === ts.SyntaxKind.PlusToken && ts.isStringLiteral(keyExpr.left) && ts.isIdentifier(keyExpr.right) && keyExpr.right.text === keyParameter.text) {
          helpers.set(name, keyExpr.left.text);
        }
      }
    };
    const visit = (node) => {
      if (ts.isFunctionDeclaration(node) && node.name) {
        registerHelper(node.name.text, node.parameters, node.body);
      }
      if (ts.isVariableDeclaration(node) && ts.isIdentifier(node.name) && node.initializer && (ts.isArrowFunction(node.initializer) || ts.isFunctionExpression(node.initializer))) {
        registerHelper(node.name.text, node.initializer.parameters, node.initializer.body);
      }
      ts.forEachChild(node, visit);
    };
    visit(tree);
    const collect = (node) => {
      if (ts.isCallExpression(node) && ts.isIdentifier(node.expression)) {
        const name = node.expression.text;
        if (name === 'labels' && node.arguments[0]) stringLiterals(node.arguments[0]).forEach((key) => keys.add(key));
        if (name === 'apiError' && node.arguments[2]) stringLiterals(node.arguments[2]).forEach((key) => keys.add(key));
        const prefix = helpers.get(name);
        if (prefix && node.arguments[1]) stringLiterals(node.arguments[1]).forEach((suffix) => keys.add(prefix + suffix));
      }
      if (ts.isJsxAttribute(node) && node.name.getText() === 'keyName' && node.initializer && ts.isStringLiteral(node.initializer)) keys.add(node.initializer.text);
      ts.forEachChild(node, collect);
    };
    collect(tree);
  }
  return keys;
}

const jsCatalog = ts.transpileModule(readFileSync(catalogPath, 'utf8'), { compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2022 } }).outputText;
const { englishUiMessages } = await import(`data:text/javascript;base64,${Buffer.from(jsCatalog).toString('base64')}`);
const used = usedUiKeys(sourceFiles(webSource));
// AppearanceAdmin derives these eight labels from the Tokens interface rather than literal calls.
for (const token of ['canvas', 'surface', 'text', 'text_muted', 'action', 'on_action', 'control_border', 'focus']) used.add(`appearanceAdmin.token.${token}`);
const missing = [...used].filter((key) => !(key in englishUiMessages)).sort();
if (missing.length) {
  console.error(`Missing English source messages for ${missing.length} UI key(s):\n${missing.map((key) => `  ${key}`).join('\n')}`);
  process.exitCode = 1;
} else {
  console.log(`English source catalog covers ${used.size} statically referenced UI keys (${Object.keys(englishUiMessages).length} total source messages).`);
}

const catalogSource = readFileSync(catalogPath, 'utf8');
const requiredOptions = ["{ tag: 'lo', label: 'ລາວ' }", "{ tag: 'en', label: 'English' }", "{ tag: 'th', label: 'ไทย' }", "{ tag: 'vi', label: 'Tiếng Việt' }", "{ tag: 'hi-IN', label: 'हिन्दी (भारत) · draft' }"];
const missingOptions = requiredOptions.filter((value) => !catalogSource.includes(value));
if (missingOptions.length) {
  console.error(`Missing required or provisional locale option(s):\n${missingOptions.join('\n')}`);
  process.exitCode = 1;
}
if (!englishUiMessages['shell.appearance.indiaHelp']?.includes('does not represent complete Indian language support')) {
  console.error('The India language sample must remain explicitly provisional until the pilot language set is approved.');
  process.exitCode = 1;
}

const draftCatalogDirectory = resolve(repositoryRoot, 'packages/localization/catalogs');
const draftCatalogFiles = readdirSync(draftCatalogDirectory).filter((name) => name.endsWith('.json')).sort();
function placeholders(text) {
  return [...text.matchAll(/\{([A-Za-z0-9_.-]+)\}/g)].map((match) => match[1]).sort();
}
let validDraftCatalogs = 0;
for (const filename of draftCatalogFiles) {
  const path = join(draftCatalogDirectory, filename);
  let draft;
  try { draft = JSON.parse(readFileSync(path, 'utf8')); }
  catch (error) {
    console.error(`Invalid JSON in ${filename}: ${error.message}`);
    process.exitCode = 1;
    continue;
  }
  const sourceKeys = Object.keys(englishUiMessages).sort();
  const draftKeys = Object.keys(draft).sort();
  const missingKeys = sourceKeys.filter((key) => !Object.hasOwn(draft, key));
  const extraKeys = draftKeys.filter((key) => !Object.hasOwn(englishUiMessages, key));
  if (missingKeys.length || extraKeys.length) {
    console.error(`${filename} key coverage mismatch: ${missingKeys.length} missing, ${extraKeys.length} extra.`);
    process.exitCode = 1;
  }
  const invalid = draftKeys.filter((key) => {
    const value = draft[key];
    return typeof value !== 'string' || !value.trim() || value.length > 10000 || /[<>]/.test(value)
      || JSON.stringify(placeholders(value)) !== JSON.stringify(placeholders(englishUiMessages[key] ?? ''));
  });
  if (invalid.length) {
    console.error(`${filename} contains ${invalid.length} blank, unsafe, non-string, or placeholder-mismatched value(s):\n${invalid.slice(0, 30).map((key) => `  ${key}`).join('\n')}`);
    process.exitCode = 1;
  }
  if (!missingKeys.length && !extraKeys.length && !invalid.length) validDraftCatalogs++;
}
console.log(`Validated ${validDraftCatalogs}/${draftCatalogFiles.length} locale draft catalogs against ${Object.keys(englishUiMessages).length} source keys. Passing catalogs are still unreviewed drafts, not approved translations.`);
