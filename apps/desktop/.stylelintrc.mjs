export default {
    extends: ['stylelint-config-standard'],
    customSyntax: 'postcss-html',
    overrides: [
        {
            files: ['**/app.css'],
            rules: {
                'color-no-hex': null,
                'function-disallowed-list': null,
            },
        },
    ],
    rules: {
        'color-no-hex': true,
        'function-disallowed-list': ['rgba', 'rgb', 'hsl', 'hsla'],
        'declaration-property-value-disallowed-list': {
            '/^(padding|margin|gap|row-gap|column-gap)(-\\w+)?$/': ['/\\d+px/'],
            'font-size': ['/\\dpx/'],
            'border-radius': ['/\\dpx/'],
            'font-family': ['/^(?!var\\(|inherit|unset|initial)/'],
        },
        'declaration-no-important': true,
        'custom-property-pattern': '^(color|spacing|font|radius|shadow|transition|z)-.+',
        'selector-class-pattern': null,
        'no-descending-specificity': null,
        'color-hex-length': null,
        'color-function-notation': null,
        'alpha-value-notation': null,
        'value-keyword-case': null,
        'property-no-vendor-prefix': null,
        'selector-pseudo-element-colon-notation': null,
        'declaration-block-no-redundant-longhand-properties': null,
        'comment-empty-line-before': null,
        'rule-empty-line-before': null,
        'selector-pseudo-class-no-unknown': [
            true,
            {
                ignorePseudoClasses: ['global'],
            },
        ],
    },
    ignoreFiles: ['dist/**', 'build/**', '.svelte-kit/**', 'node_modules/**', 'src-tauri/target/**', 'target/**'],
}
