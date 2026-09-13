/**
 * 剪贴板图片 → 待发送附件（截图粘贴链路的纯函数部分）。
 *
 * Linux 的 WebKitGTK 不把剪贴板图片暴露给网页（paste 事件里没有图），
 * 后端从 GTK 剪贴板读到 PNG base64 后，由这里组装成与网页端
 * takeImageFile 同构的 pendingImg，供输入区预览并按 Enter 发送。
 */

/** base64 → Blob（Node/browser 通用的 Uint8Array 转换） */
export function b64ToBlob(b64, mime = 'image/png') {
  const bin = atob(b64)
  const bytes = new Uint8Array(bin.length)
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i)
  return new Blob([bytes], { type: mime })
}

/**
 * 组装待发送图片对象。
 * @param {string} b64
 * @param {string} [mime='image/png']
 * @param {number} [size] 后端下发的原始字节数；缺省按解码长度
 * @returns {{b64:string, mime:string, size:number, blob:Blob}}
 */
export function pendingImgFromB64(b64, mime = 'image/png', size) {
  const blob = b64ToBlob(b64, mime)
  return { b64, mime, size: size ?? blob.size, blob }
}

/** base64 → Uint8Array（写剪贴板图片时给 Tauri 的 Image.fromBytes 用） */
export function b64ToBytes(b64) {
  const bin = atob(b64 || '')
  const bytes = new Uint8Array(bin.length)
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i)
  return bytes
}