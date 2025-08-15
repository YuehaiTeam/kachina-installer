import DOMPurify from 'dompurify';

// 安全的CSS属性白名单（SVG图形属性 + 基础布局样式）
const SAFE_CSS_PROPERTIES = [
  // SVG专用属性
  'fill', 'stroke', 'stroke-width', 'stroke-dasharray', 'stroke-dashoffset',
  'opacity', 'fill-opacity', 'stroke-opacity', 'visibility',
  'transform', 'transform-origin', 'clip-path', 'mask',
  
  // 基础布局和间距
  'margin', 'margin-top', 'margin-right', 'margin-bottom', 'margin-left',
  'padding', 'padding-top', 'padding-right', 'padding-bottom', 'padding-left',
  'width', 'height', 'max-width', 'max-height', 'min-width', 'min-height',
  
  // 显示和定位（限制范围）
  'display', 'overflow', 'box-sizing',
  
  // 颜色和文本
  'color', 'background-color', 'border-color',
  'font-size', 'font-weight', 'font-family', 'text-align',
  
  // 边框
  'border', 'border-width', 'border-style', 'border-radius',
  'border-top', 'border-right', 'border-bottom', 'border-left'
];

function sanitizeCssStyle(styleValue: string): string {
  if (!styleValue) return '';
  
  // 移除危险的CSS函数和关键字
  const dangerousPatterns = [
    /javascript:/gi,
    /expression\s*\(/gi,
    /url\s*\(/gi,
    /import/gi,
    /@/gi, // 移除CSS at-rules
    /behaviour:/gi,
    /-moz-binding:/gi
  ];
  
  for (const pattern of dangerousPatterns) {
    if (pattern.test(styleValue)) {
      return ''; // 发现危险内容，返回空字符串
    }
  }
  
  // 解析CSS属性并过滤
  const declarations = styleValue.split(';')
    .map(decl => decl.trim())
    .filter(decl => decl)
    .filter(decl => {
      const [property] = decl.split(':').map(part => part.trim());
      return SAFE_CSS_PROPERTIES.includes(property.toLowerCase());
    });
  
  return declarations.join('; ');
}

export function sanitizeSvg(svgContent: string): string | null {
  if (
    !svgContent?.trim().startsWith('<svg') ||
    !svgContent.includes('</svg>')
  ) {
    return null;
  }

  try {
    // 首先用DOMPurify进行基础清理
    let cleaned = DOMPurify.sanitize(svgContent, {
      USE_PROFILES: { svg: true },
      ALLOWED_TAGS: [
        'svg',
        'path',
        'g',
        'rect',
        'circle',
        'ellipse',
        'line',
        'polyline',
        'polygon',
      ],
      ALLOWED_ATTR: [
        'viewBox',
        'width',
        'height',
        'fill',
        'stroke',
        'stroke-width',
        'd',
        'x',
        'y',
        'cx',
        'cy',
        'r',
        'rx',
        'ry',
        'transform',
        'style',
        'class',
      ],
      FORBID_TAGS: ['script', 'iframe', 'object', 'embed'],
      FORBID_ATTR: ['onload', 'onclick', 'onmouseover'],
    });

    if (!cleaned) return null;

    // 进一步净化style属性
    cleaned = cleaned.replace(/style\s*=\s*["']([^"']*)["']/gi, (match, styleValue) => {
      const safeCss = sanitizeCssStyle(styleValue);
      return safeCss ? `style="${safeCss}"` : '';
    });

    return cleaned;
  } catch {
    return null;
  }
}
