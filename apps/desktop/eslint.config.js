import js from '@eslint/js'
import prettierConfig from 'eslint-config-prettier'
import tseslint from 'typescript-eslint'
import svelte from 'eslint-plugin-svelte'
import svelteParser from 'svelte-eslint-parser'
import globals from 'globals'

export default tseslint.config(
    {
        ignores: ['dist', 'build', '.svelte-kit', 'node_modules', 'src-tauri/target', 'target'],
    },
    js.configs.recommended,
    prettierConfig,
    ...tseslint.configs.strict.map((config) => ({
        ...config,
        files: ['**/*.{ts,tsx,svelte.ts,svelte}'],
    })),
    ...svelte.configs['flat/recommended'],
    {
        files: ['**/*.{ts,tsx,svelte.ts}'],
        plugins: {
            '@typescript-eslint': tseslint.plugin,
        },
        languageOptions: {
            ecmaVersion: 'latest',
            sourceType: 'module',
            globals: {
                ...globals.browser,
                ...globals.node,
                ...globals.es2021,
            },
        },
        rules: {
            '@typescript-eslint/no-unused-vars': 'error',
            '@typescript-eslint/no-explicit-any': 'error',
            'no-console': ['warn', { allow: ['error', 'warn'] }],
            complexity: ['error', { max: 15 }],
        },
    },
    {
        files: ['scripts/*.js', 'vite.config.js'],
        languageOptions: {
            ecmaVersion: 'latest',
            sourceType: 'module',
            globals: {
                ...globals.node,
            },
        },
    },
    {
        files: ['**/*.svelte'],
        languageOptions: {
            parser: svelteParser,
            parserOptions: {
                parser: tseslint.parser,
                extraFileExtensions: ['.svelte'],
            },
        },
        rules: {
            '@typescript-eslint/no-unused-vars': 'error',
            'no-console': ['warn', { allow: ['error', 'warn'] }],
            complexity: ['error', { max: 15 }],
        },
    },
)
