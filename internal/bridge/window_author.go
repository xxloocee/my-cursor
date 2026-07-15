package bridge

import "github.com/pkg/browser"

const footerAuthorHomeURL = "https://ergouzi.life"

var footerAuthorInfo = FooterAuthorInfo{
	ButtonText:        "Ergouzi",
	DialogTitle:       "关于 Ergouzi",
	DialogContent:     "My Cursor 提供 Cursor 本地服务与自定义模型 API 管理能力。\n访问 https://ergouzi.life 了解更多内容。",
	DialogConfirmText: "访问 Ergouzi",
	DialogCancelText:  "关闭",
}

// FooterAuthorInfo 定义首页底部作者入口的展示信息。
type FooterAuthorInfo struct {
	ButtonText        string `json:"buttonText"`
	DialogTitle       string `json:"dialogTitle"`
	DialogContent     string `json:"dialogContent"`
	DialogConfirmText string `json:"dialogConfirmText"`
	DialogCancelText  string `json:"dialogCancelText"`
}

// GetFooterAuthorInfo 返回首页底部作者入口的展示信息。
func (s *WindowService) GetFooterAuthorInfo() FooterAuthorInfo {
	return footerAuthorInfo
}

// OpenFooterAuthorHome 打开作者主页。
func (s *WindowService) OpenFooterAuthorHome() error {
	return browser.OpenURL(footerAuthorHomeURL)
}
