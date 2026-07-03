{{/*
Expand the name of the chart.
*/}}
{{- define "edger.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Create a default fully qualified app name.
*/}}
{{- define "edger.fullname" -}}
{{- if .Values.fullnameOverride }}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- $name := default .Chart.Name .Values.nameOverride }}
{{- if contains $name .Release.Name }}
{{- .Release.Name | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- printf "%s-%s" .Release.Name $name | trunc 63 | trimSuffix "-" }}
{{- end }}
{{- end }}
{{- end }}

{{/*
Common labels.
*/}}
{{- define "edger.labels" -}}
app: {{ include "edger.name" . }}
app.kubernetes.io/name: {{ include "edger.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
helm.sh/chart: {{ printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" }}
{{- end }}

{{/*
Selector labels.
*/}}
{{- define "edger.selectorLabels" -}}
app: {{ include "edger.name" . }}
app.kubernetes.io/name: {{ include "edger.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{/*
Secret name that contains the file-backed root key.
*/}}
{{- define "edger.rootKeySecretName" -}}
{{- if .Values.rootKey.existingSecret -}}
{{- .Values.rootKey.existingSecret -}}
{{- else -}}
{{- printf "%s-root-key" (include "edger.fullname" .) -}}
{{- end -}}
{{- end }}

{{/*
Absolute path passed to EDGER_ROOT_KEY_FILE.
*/}}
{{- define "edger.rootKeyFilePath" -}}
{{- printf "%s/%s" .Values.rootKey.mountPath .Values.rootKey.fileName -}}
{{- end }}
